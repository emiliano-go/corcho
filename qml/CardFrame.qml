import QtQuick
import "OverlayState.js" as OverlayState

FocusScope {
    id: root

    property var app
    property var th
    property var card
    // The owning CanvasView, used by bodies that need to override global UI.
    property var canvasView: null
    readonly property var settings: {
        try { return JSON.parse(root.app.settingsJson); } catch (e) { return ({}); }
    }
    property string gridId: ""
    property bool selected: false
    property bool resizable: true
    property bool showChrome: true
    // True when the card is rendered inside a picture-in-picture window. It
    // fills its parent instead of following the model geometry, and the
    // canvas-only drag, resize and selection gestures are disabled.
    property bool detached: false
    // True for a card that is shown in a picture-in-picture window while this
    // canvas delegate stays behind as a greyed-out placeholder.
    property bool floating: false
    // The picture-in-picture window paints a single rounded surface behind
    // the whole card, so the detached copy hides its own card surface.
    property bool surfaceless: false
    signal returnRequested()
    default property alias bodyData: body.data

    signal selectRequested(bool additive, real sceneX, real sceneY)
    signal movedBy(real dx, real dy)
    Accessible.role: Accessible.Grouping
    Accessible.name: (root.card ? (root.card.title || root.card.type) : "Card") + " card"
    signal menuRequested(var card, real globalX, real globalY)
    signal dateLinkRequested(string sourceCardId, string sourceItemId, string linkId, string defaultDate)
    signal calendarManagerRequested()
    property bool cardHovered: false
    property bool menuHovered: false
    property bool resizeHovered: false
    // Set while a control inside the card is pressed. Its tap must run the
    // action only; it must not also select and raise the card.
    property bool suppressTapSelection: false

    function beginActionPress() {
        root.suppressTapSelection = true;
    }

    function endActionPress() {
        Qt.callLater(function () { root.suppressTapSelection = false; });
    }
    // Bodies can override the cursor shown while the card is draggable.
    property int dragCursorShape: Qt.SizeAllCursor
    property real parallaxX: 0
    property real parallaxY: 0
    // Set when a drag/resize starts so pointer handlers on cards that are
    // covered by another card can ignore the gesture.
    property bool dragAllowed: true
    property bool resizeAllowed: true

    x: detached ? 0 : (card ? card.x : 0)
    y: detached ? 0 : (card ? card.y : 0)
    width: detached ? (parent ? parent.width : 320) : (card ? card.width : 320)
    height: detached ? (parent ? parent.height : 240) : (card ? card.height : 240)
    z: selected ? 20 : (root.cardHovered ? 10 : 1)
    property bool entered: false
    property bool editingTitle: false
    readonly property real minimumWidth: {
        var t = root.card ? root.card.type : "";
        if (t === "calendar") return 420;
        if (t === "kanban" || t === "git") return 360;
        if (t === "website" || t === "ai" || t === "whiteboard") return 320;
        if (t === "file" || t === "checklist") return 300;
        return 240;
    }
    readonly property real minimumHeight: {
        var t = root.card ? root.card.type : "";
        if (t === "calendar") return 500;
        if (t === "git" || t === "kanban") return 260;
        if (t === "website" || t === "whiteboard") return 220;
        if (t === "ai") return 300;
        return 180;
    }
    // Corner radius of the card surface, shared by the surface, the
    // picture-in-picture placeholder and the resize grip.
    readonly property real cardRadius: Math.max(0, Math.min(28,
        root.settings && root.settings.card_radius !== undefined
            ? root.settings.card_radius
            : (root.th ? root.th.radiusCard : 8)))
    // The grip icon has to stay inside the rounded corner (the card must
    // always cover it) while leaving the outer 16px of the card free, where
    // the content scrollbars and their arrow buttons live.
    readonly property real gripInset: Math.ceil(root.cardRadius * 0.3)
    readonly property real gripSize: Math.max(6, 16 - root.gripInset)

    function enter() {
        if (entered) return;
        entered = true;
        if (root.settings && root.settings.spawn_animations && !root.settings.reduced_motion) {
            opacity = 0;
            enterAnim.start();
        }
    }

    onSettingsChanged: enter()

    function updateParallax(px, py) {
        if (root.detached || !root.settings || !root.settings.card_parallax || root.settings.reduced_motion) {
            parallaxX = 0;
            parallaxY = 0;
            return;
        }
        parallaxX = (px / Math.max(1, width) - 0.5) * -8;
        parallaxY = (py / Math.max(1, height) - 0.5) * -8;
    }

    onCardChanged: {
        root.editingTitle = false;
        if (!root.detached && !mover.active && !resizer.active) {
            x = card.x;
            y = card.y;
            width = card.width;
            height = card.height;
        }
        if (!titleInput.activeFocus) titleInput.text = card.title || "";
    }
    Component.onCompleted: enter()

    NumberAnimation {
        id: enterAnim
        target: root
        property: "opacity"
        to: 1
        duration: root.th.motionSlow
        easing.type: Easing.OutCubic
    }

    function commit(payloadPatch, textValue) {
        var payload = {};
        if (payloadPatch) {
            for (var k in payloadPatch) payload[k] = payloadPatch[k];
        }
        // A detached card fills its window, not its canvas slot. Keep the
        // canvas geometry so editing the floating copy never teleports or
        // resizes the card that stays behind.
        var keepGeometry = root.detached && root.card;
        app.updateCard(JSON.stringify({
            id: card.id,
            x: keepGeometry ? root.card.x : root.x,
            y: keepGeometry ? root.card.y : root.y,
            width: keepGeometry ? root.card.width : root.width,
            height: keepGeometry ? root.card.height : root.height,
            title: titleInput.text,
            text: textValue !== undefined && textValue !== null ? textValue : root.card.text,
            payload: payload
        }));
    }

    function snapValue(value) {
        return (root.settings && root.settings.grid_magnetic) ? Math.round(value / 32) * 32 : value;
    }

    // Text editors inside card bodies consume mouse presses for caret
    // placement, so the card's own TapHandler never sees those clicks. Bodies
    // call this from a TapHandler on their editor to select the card.
    // `modifiers` must come from the handler's point (a QQuickHandlerPoint),
    // because the tapped signal argument is a QEventPoint without modifiers.
    //
    // A tap can reach both the card handler and a body handler; the first one
    // wins so an additive click doesn't toggle the selection twice.
    property bool tapSelectHandled: false

    function applyTapSelection(modifiers, sceneX, sceneY) {
        if (tapSelectHandled || root.suppressTapSelection || OverlayState.openMenus > 0)
            return;
        tapSelectHandled = true;
        // Clear after the current event loop so the other handler that saw the
        // same tap is ignored, while the next click starts fresh.
        Qt.callLater(function () { root.tapSelectHandled = false; });
        root.selectRequested(!!(modifiers & Qt.ControlModifier), sceneX, sceneY);
    }

    function requestBodySelect(modifiers, sceneX, sceneY) {
        root.applyTapSelection(modifiers, sceneX, sceneY);
    }

    function commitGeometry() {
        if (root.detached) return;
        if (root.settings && root.settings.grid_magnetic) {
            root.x = snapValue(root.x);
            root.y = snapValue(root.y);
        }
        root.width = Math.max(root.minimumWidth, root.settings && root.settings.grid_magnetic ? snapValue(root.width) : root.width);
        root.height = Math.max(root.minimumHeight, root.settings && root.settings.grid_magnetic ? snapValue(root.height) : root.height);
        app.updateCard(JSON.stringify({
            id: card.id,
            x: root.x,
            y: root.y,
            width: root.width,
            height: root.height
        }));
    }

    Rectangle {
        id: surface
        anchors.fill: parent
        visible: !root.surfaceless
        radius: root.cardRadius
        transform: Translate { x: root.parallaxX; y: root.parallaxY }
        color: root.th.card
        border.width: root.selected ? root.th.borderWidthStrong : root.th.borderWidth
        border.color: root.selected ? root.th.accent : (root.cardHovered ? root.th.borderStrong : root.th.border)
        Behavior on border.color { ColorAnimation { duration: 120 } }
    }

    // Consume left-button presses on the card's plain surface so they cannot
    // fall through to cards stacked behind this one. Interactive children are
    // declared above and keep their own input handling.
    MouseArea {
        id: pointerShield
        anchors.fill: parent
        acceptedButtons: Qt.LeftButton
        preventStealing: false
        enabled: !root.floating
        onClicked: function (mouse) {
            mouse.accepted = true;
            var scene = mapToItem(null, mouse.x, mouse.y);
            root.applyTapSelection(mouse.modifiers, scene.x, scene.y);
        }
    }

    Item {
        visible: root.selected
        width: 6
        height: 10
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.top: parent.top
        anchors.topMargin: -3
        z: 2

        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            width: 6
            height: 6
            radius: root.th.radiusSmall
            color: root.card.color || root.th.accent
            border.width: root.th.borderWidth
            border.color: root.th.accent
        }
        Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.top: parent.top
            anchors.topMargin: 6
            width: 1
            height: 4
            color: root.th.accent
            opacity: root.th.opacityEmphasis
        }
    }

    Item {
        id: header
        visible: root.showChrome
        enabled: !root.floating
        transform: Translate { x: root.parallaxX; y: root.parallaxY }
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        height: root.th ? root.th.cardHeaderHeight : 44

        Row {
            anchors.left: parent.left
            anchors.leftMargin: root.th.space4
            anchors.right: menuButton.left
            anchors.rightMargin: root.th.space2
            anchors.verticalCenter: parent.verticalCenter
            spacing: root.th.space2
            Icon {
                anchors.verticalCenter: parent.verticalCenter
                name: root.card.icon || "square"
                size: root.th.iconBody
                color: root.th.text2
            }

            Text {
                id: titleLabel
                width: parent.width - 26
                anchors.verticalCenter: parent.verticalCenter
                visible: !root.editingTitle
                text: root.card.title || "Untitled"
                color: root.card.title ? root.th.text : root.th.muted
                font.family: root.th.fontFamily
                font.pixelSize: root.th.fontSizeBody
                font.weight: root.th.fontWeightMedium
                elide: Text.ElideRight
            }

            TextInput {
                id: titleInput
                width: parent.width - 26
                anchors.verticalCenter: parent.verticalCenter
                visible: root.editingTitle
                text: root.card.title || ""
                color: root.th.text
                font.family: root.th.fontFamily
                font.pixelSize: root.th.fontSizeBody
                font.weight: root.th.fontWeightMedium
                selectByMouse: true
                selectionColor: root.th.accent
                clip: true
                onEditingFinished: {
                    root.commit(null);
                    root.editingTitle = false;
                }
                Keys.onEscapePressed: {
                    text = root.card.title || "";
                    root.editingTitle = false;
                    focus = false;
                }
            }
        }

        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.LeftButton
            // Let the DragHandler take over once the pointer moves, otherwise
            // the mouse area keeps the grab and the card cannot be moved.
            preventStealing: false
            onClicked: function (mouse) {
                mouse.accepted = true;
                var scene = mapToItem(null, mouse.x, mouse.y);
                root.applyTapSelection(mouse.modifiers, scene.x, scene.y);
            }
            onDoubleClicked: function (mouse) {
                mouse.accepted = true;
                var scene = mapToItem(null, mouse.x, mouse.y);
                if (root.canvasView && root.card
                        && !root.canvasView.isTopCardAtScene(root.card.id, scene.x, scene.y))
                    return;
                root.editingTitle = true;
                Qt.callLater(function () {
                    titleInput.forceActiveFocus();
                    titleInput.selectAll();
                });
            }
        }

        Icon {
            visible: root.card && !!root.card.syncId
            anchors.right: menuButton.left
            anchors.rightMargin: root.card && root.card.locked ? 26 : 6
            anchors.verticalCenter: parent.verticalCenter
            name: "link"
            size: root.th.iconSmall
            color: root.th.accent
        }

        Icon {
            visible: root.card && root.card.locked
            anchors.right: menuButton.left
            anchors.rightMargin: 6
            anchors.verticalCenter: parent.verticalCenter
            name: "pin"
            size: root.th.iconSmall
            color: root.th.accent
        }

        Item {
            id: menuButton
            width: 32
            height: 32
            anchors.right: parent.right
            anchors.rightMargin: root.th.space3
            anchors.verticalCenter: parent.verticalCenter
            opacity: root.cardHovered || root.selected ? 1 : 0

            Rectangle {
                anchors.fill: parent
                radius: root.th.radiusControl
                color: root.menuHovered ? root.th.hover : "transparent"
            }
            Icon {
                anchors.centerIn: parent
                name: "ellipsis"
                size: root.th.iconBody
                color: root.th.text2
            }
            HoverHandler { id: menuHover
                onHoveredChanged: root.menuHovered = hovered
            }
            CardTapHandler {
                onAction: {
                    var p = root.mapToItem(null, root.width - 20, 28);
                    root.menuRequested(root.card, p.x, p.y);
                }
            }
        }

        DragHandler {
            id: mover
            target: null
            enabled: !root.card.locked && !root.editingTitle && !root.floating && !root.detached
                && !(root.canvasView && root.canvasView.linkSourceId !== "")
            grabPermissions: PointerHandler.CanTakeOverFromAnything
            cursorShape: root.dragCursorShape
            property real lastX: 0
            property real lastY: 0
            property real totalX: 0
            property real totalY: 0
            onActiveChanged: {
                if (active) {
                    dragAllowed = !root.canvasView || !root.card
                        || root.canvasView.isTopCardAtScene(
                            root.card.id, centroid.scenePosition.x, centroid.scenePosition.y);
                    lastX = 0;
                    lastY = 0;
                    totalX = 0;
                    totalY = 0;
                    if (dragAllowed && root.canvasView)
                        root.canvasView.beginCardDrag(root.card.id);
                    if (!root.selected && dragAllowed)
                        root.selectRequested(false, centroid.scenePosition.x, centroid.scenePosition.y);
                } else {
                    if (dragAllowed) {
                        root.commitGeometry();
                        if (Math.abs(totalX) > 0.5 || Math.abs(totalY) > 0.5)
                            root.movedBy(totalX, totalY);
                        if (root.canvasView)
                            root.canvasView.endCardDrag();
                    }
                    dragAllowed = true;
                }
            }
            onTranslationChanged: {
                if (!dragAllowed) return;
                var z = Math.max(0.2, root.app.zoom);
                var dx = (translation.x - lastX) / z;
                var dy = (translation.y - lastY) / z;
                root.x += dx;
                root.y += dy;
                totalX += dx;
                totalY += dy;
                if (root.canvasView)
                    root.canvasView.updateCardDrag(totalX, totalY);
                lastX = translation.x;
                lastY = translation.y;
            }
        }


    }

    Item {
        id: body
        transform: Translate { x: root.parallaxX; y: root.parallaxY }
        enabled: !root.floating
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: root.showChrome ? header.bottom : parent.top
        anchors.bottom: parent.bottom
        anchors.margins: root.showChrome ? 16 : 12
        anchors.topMargin: root.showChrome ? 0 : 12
        clip: true
    }

    Item {
        id: resizeHandle
        visible: root.resizable && !root.card.locked && !root.floating && !root.detached
        // The hit area covers the whole corner, but stops where the content
        // scrollbars begin (16px in from the card edge), so their down arrow
        // stays clickable.
        width: 16
        height: 16
        z: 10
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.rightMargin: 0
        anchors.bottomMargin: 0
        opacity: root.resizeHovered || root.selected ? 1 : 0

        // Pulled in from the corner far enough that the rounded card surface
        // always covers it: at radius r the corner is cut back by r * (1 -
        // 1/sqrt(2)) ~= 0.29 * r.
        Icon {
            objectName: "resizeGrip"
            anchors.right: parent.right
            anchors.bottom: parent.bottom
            anchors.rightMargin: root.gripInset
            anchors.bottomMargin: root.gripInset
            name: "maximize-2"
            size: root.gripSize
            color: root.th.muted
            rotation: 90
        }

        Rectangle {
            id: resizeTip
            anchors.right: parent.right
            anchors.bottom: parent.top
            anchors.bottomMargin: 6
            width: resizeTipText.implicitWidth + 16
            height: 24
            radius: root.th.radiusKeycap
            color: root.th.menu
            border.width: 1
            border.color: root.th.border
            visible: resizeHover.hovered
            Text {
                id: resizeTipText
                anchors.centerIn: parent
                text: "Ctrl + right-click to resize"
                color: root.th.text2
                font.family: root.th.fontFamily
                font.pixelSize: root.th.fontSizeSmall
            }
        }

        HoverHandler { id: resizeHover
            onHoveredChanged: root.resizeHovered = hovered
        }
        DragHandler {
            id: resizer
            target: null
            cursorShape: Qt.SizeFDiagCursor
            property real lastX: 0
            property real lastY: 0
            onActiveChanged: {
                if (active) {
                    resizeAllowed = !root.canvasView || !root.card
                        || root.canvasView.isTopCardAtScene(
                            root.card.id, centroid.scenePosition.x, centroid.scenePosition.y);
                    lastX = 0;
                    lastY = 0;
                    if (resizeAllowed)
                        root.selectRequested(false, centroid.scenePosition.x, centroid.scenePosition.y);
                } else {
                    if (resizeAllowed) root.commitGeometry();
                    resizeAllowed = true;
                }
            }
            onTranslationChanged: {
                if (!resizeAllowed) return;
                var z = Math.max(0.2, root.app.zoom);
                root.width = Math.max(root.minimumWidth, root.width + (translation.x - lastX) / z);
                root.height = Math.max(root.minimumHeight, root.height + (translation.y - lastY) / z);
                lastX = translation.x;
                lastY = translation.y;
            }
        }
    }

    HoverHandler { id: hover
        onHoveredChanged: {
            root.cardHovered = hovered;
            if (!hovered) {
                root.parallaxX = 0;
                root.parallaxY = 0;
            }
        }
        onPointChanged: {
            if (root.canvasView && root.card
                    && !root.canvasView.isTopCardAtScene(
                        root.card.id, point.scenePosition.x, point.scenePosition.y)) {
                root.parallaxX = 0;
                root.parallaxY = 0;
                return;
            }
            root.updateParallax(point.position.x, point.position.y);
        }
    }

    TapHandler {
        acceptedButtons: Qt.LeftButton
        enabled: !root.floating && !root.detached
        onTapped: root.applyTapSelection(
            point.modifiers, point.scenePosition.x, point.scenePosition.y)
    }

    MouseArea {
        id: rightButton
        anchors.fill: parent
        z: 9
        acceptedButtons: Qt.RightButton
        enabled: !root.detached
        property bool resizing: false
        property bool menuPending: false
        property real startW: 0
        property real startH: 0
        property real startX: 0
        property real startY: 0

        onPressed: function (mouse) {
            if (!root.card) return;
            var scene = root.mapToItem(null, mouse.x, mouse.y);
            if (root.canvasView
                    && !root.canvasView.isTopCardAtScene(root.card.id, scene.x, scene.y))
                return;
            if (root.card.locked) {
                menuPending = true;
                mouse.accepted = true;
                return;
            }
            if (mouse.modifiers & Qt.ControlModifier) {
                resizing = true;
                menuPending = false;
                startW = root.width;
                startH = root.height;
                startX = mouse.x;
                startY = mouse.y;
                root.selectRequested(false, scene.x, scene.y);
                mouse.accepted = true;
            } else {
                resizing = false;
                menuPending = true;
                mouse.accepted = true;
            }
        }
        onPositionChanged: function (mouse) {
            if (!resizing) return;
            var z = Math.max(0.2, root.app ? root.app.zoom : 1);
            root.width = Math.max(root.minimumWidth, startW + (mouse.x - startX) / z);
            root.height = Math.max(root.minimumHeight, startH + (mouse.y - startY) / z);
        }
        onReleased: function (mouse) {
            if (resizing) {
                resizing = false;
                root.commitGeometry();
                mouse.accepted = true;
            } else if (menuPending) {
                menuPending = false;
                var p = root.mapToItem(null, root.width - 20, 28);
                root.menuRequested(root.card, p.x, p.y);
                mouse.accepted = true;
            }
        }
        onCanceled: {
            resizing = false;
            menuPending = false;
        }
    }

    // Placeholder shown on the canvas while the card lives in its own
    // picture-in-picture window. The return action appears on hover only.
    Rectangle {
        id: floatingShade
        objectName: "floatingShade"
        anchors.fill: parent
        visible: root.floating
        z: 40
        radius: root.cardRadius
        color: Qt.rgba(root.th.bg.r, root.th.bg.g, root.th.bg.b, 0.58)
        border.width: root.th.borderWidth
        border.color: root.th.border

        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.LeftButton
            onClicked: function (mouse) { mouse.accepted = true; }
        }

        HoverHandler { id: shadeHover }

        AppButton {
            anchors.centerIn: parent
            visible: root.cardHovered || shadeHover.hovered
            th: root.th
            icon: "undo-2"
            text: "Back card to canvas"
            variant: "solid"
            onClicked: root.returnRequested()
        }
    }
}
