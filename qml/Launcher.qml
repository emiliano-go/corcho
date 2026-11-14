import QtQuick

Item {
    id: root

    property var app
    property var th
    property bool opened: false
    property int selectedIndex: 0

    signal closed()
    // Emitted when a command created a note or checklist card so the canvas
    // can put the caret in its editor.
    signal cardCreated(string cardId)
    property bool rowHovered: false

    anchors.fill: parent
    visible: opened
    z: 400

    function open() {
        opened = true;
        selectedIndex = 0;
        query.text = "";
        rebuild();
        // Focus the inner TextInput, not the AppTextField wrapper. The wrapper
        // is only an Item, so focusing it leaves the query unable to receive
        // typing until the user clicks the field.
        Qt.callLater(function () {
            if (!root.opened) return;
            query.input.forceActiveFocus();
            query.input.selectAll();
        });
    }

    function close() {
        opened = false;
        root.closed();
    }

    readonly property var commands: [
        { id: "create-note", label: "Add note", icon: "file-text", keywords: "note card" },
        { id: "create-website", label: "Add website", icon: "globe", keywords: "web embed" },
        { id: "create-bookmark", label: "Add bookmark", icon: "bookmark", keywords: "link" },
        { id: "create-api", label: "Add API data", icon: "database", keywords: "http json" },
        { id: "create-ai", label: "Add AI node", icon: "cpu", keywords: "ai prompt" },
        { id: "create-calendar", label: "Add calendar", icon: "calendar", keywords: "dates reminders" },
        { id: "create-timbre", label: "Add timbre", icon: "audio-lines", keywords: "sound audio noise tone" },
        { id: "create-script", label: "Add script", icon: "terminal", keywords: "shell" },
        { id: "create-checklist", label: "Add checklist", icon: "list-checks", keywords: "todo tasks" },
        { id: "create-kanban", label: "Add kanban", icon: "kanban", keywords: "board columns" },
        { id: "create-whiteboard", label: "Add whiteboard", icon: "pencil", keywords: "draw" },
        { id: "create-legend", label: "Add legend", icon: "list", keywords: "arrows connectors meaning" },
        { id: "create-pomodoro", label: "Add pomodoro", icon: "timer", keywords: "timer focus" },
        { id: "create-mpris", label: "Add now playing", icon: "music-2", keywords: "music media" },
        { id: "create-grid", label: "Create grid", icon: "layout-grid", keywords: "new grid" },
        { id: "create-folder", label: "Create folder", icon: "folder-plus", keywords: "new folder" },
        { id: "undo", label: "Undo", icon: "undo-2", keywords: "history" },
        { id: "redo", label: "Redo", icon: "rotate-cw", keywords: "history" },
        { id: "settings", label: "Open settings", icon: "settings", keywords: "preferences" }
    ]

    ListModel { id: results }

    function rebuild() {
        var q = query.text.trim().toLowerCase();
        results.clear();
        var found = 0;

        var grids = [];
        try { grids = JSON.parse(app.gridsJson); } catch (e) {}
        for (var i = 0; i < grids.length && found < 8; i++) {
            if (q === "" || grids[i].name.toLowerCase().indexOf(q) >= 0) {
                results.append({ kind: "grid", id: grids[i].id, gridId: grids[i].id, label: grids[i].name, detail: grids[i].cards.length + " cards", icon: "layout-grid" });
                found++;
            }
        }

        var cards = [];
        try { cards = JSON.parse(app.search(q)); } catch (e) {}
        for (var j = 0; j < cards.length && found < 30; j++) {
            var c = cards[j];
            if (c.type === "card") {
                results.append({ kind: "card", id: c.id, gridId: c.gridId, label: c.label, detail: c.gridName, icon: c.icon || "square" });
                found++;
            }
        }

        for (var k = 0; k < commands.length && found < 50; k++) {
            var cmd = commands[k];
            var hay = cmd.label.toLowerCase() + " " + cmd.keywords;
            if (q === "" || hay.indexOf(q) >= 0) {
                results.append({ kind: "command", id: cmd.id, label: cmd.label, detail: "", icon: cmd.icon });
                found++;
            }
        }

        selectedIndex = found > 0 ? 0 : -1;
        list.positionViewAtBeginning();
    }

    function run(index) {
        if (index < 0 || index >= results.count) return;
        var item = results.get(index);
        if (item.kind === "grid") {
            app.switchGrid(item.gridId);
        } else if (item.kind === "card") {
            app.switchGrid(item.gridId);
        } else if (item.kind === "command") {
            if (item.id === "settings") {
                root.close();
                root.settingsRequested();
                return;
            }
            var created = null;
            try { created = JSON.parse(String(app.command(item.id))); } catch (e) {}
            if (created && created.id
                    && (created.type === "note" || created.type === "checklist"))
                root.cardCreated(created.id);
        }
        root.close();
    }

    function scrollResults(delta) {
        var maxY = Math.max(0, list.contentHeight - list.height);
        list.contentY = Math.max(0, Math.min(maxY, list.contentY - delta));
    }

    function wheelStep(event) {
        return event.pixelDelta.y !== 0
            ? event.pixelDelta.y
            : event.angleDelta.y / 120 * Math.max(48, root.th.menuItemHeight * 2);
    }

    signal settingsRequested()

    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.55)
        MouseArea {
            anchors.fill: parent
            onClicked: root.close()
            onWheel: function (wheel) { wheel.accepted = true; }
        }
    }

    Rectangle {
        id: panel
        width: Math.min(root.th.launcherMaxWidth, root.width - root.th.space6)
        height: root.th.launcherHeight
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.top: parent.top
        anchors.topMargin: root.th.launcherTopMargin
        radius: root.th.radiusPanel
        color: root.th.menu
        border.width: root.th.borderWidth
        border.color: root.th.border

        MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.NoButton
            onWheel: function (wheel) {
                root.scrollResults(root.wheelStep(wheel));
                wheel.accepted = true;
            }
        }

        Column {
            anchors.fill: parent
            anchors.margins: root.th.space4
            spacing: root.th.space3
            AppTextField {
                id: query
                objectName: "launcherQuery"
                width: parent.width
                inputHeight: 42
                th: root.th
                placeholder: "Search"
                onEdited: root.rebuild()
                onAccepted: root.run(root.selectedIndex)
                onKeyPressed: function (event) {
                    if (event.key === Qt.Key_Down) {
                        root.selectedIndex = Math.min(results.count - 1, root.selectedIndex + 1);
                        event.accepted = true;
                    } else if (event.key === Qt.Key_Up) {
                        root.selectedIndex = Math.max(0, root.selectedIndex - 1);
                        event.accepted = true;
                    } else if (event.key === Qt.Key_Escape) {
                        root.close();
                        event.accepted = true;
                    }
                }
            }

            ListView {
                id: list
                width: parent.width
                height: parent.height - query.height - root.th.space3
                clip: true
                spacing: root.th.spaceHalf
                model: results
                currentIndex: root.selectedIndex

                // Scroll the results here and always consume the wheel. A
                // Flickable lets the event bubble to the canvas once it reaches
                // its end, which made the grid pan while the launcher was open.
                WheelHandler {
                    acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchPad
                    onWheel: function (event) {
                        root.scrollResults(root.wheelStep(event));
                        event.accepted = true;
                    }
                }

                delegate: Item {
                    id: resultRow
                    required property var modelData
                    required property int index
                    property bool hoveredNow: false

                    width: list.width
                    height: root.th.menuItemHeight

                    Rectangle {
                        anchors.fill: parent
                        radius: root.th.radiusControl
                        color: index === root.selectedIndex ? root.th.surfaceStrong : (resultRow.hoveredNow ? root.th.hover : "transparent")
                    }

                    Row {
                        anchors.fill: parent
                        anchors.leftMargin: root.th.space3
                        anchors.rightMargin: root.th.space3
                        spacing: root.th.space3
                        Icon {
                            anchors.verticalCenter: parent.verticalCenter
                            name: modelData.icon || "square"
                            size: root.th.iconBody
                            color: root.th.text2
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            width: parent.width - 120
                            text: modelData.label
                            color: root.th.text
                            elide: Text.ElideRight
                            font.family: root.th.fontFamily
                            font.pixelSize: root.th.fontSizeBody
                        }
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            width: 90
                            text: modelData.detail
                            color: root.th.muted
                            horizontalAlignment: Text.AlignRight
                            elide: Text.ElideRight
                            font.family: root.th.fontFamily
                            font.pixelSize: root.th.fontSizeSmall
                        }
                    }

                    HoverHandler { id: rowHover
                        onHoveredChanged: resultRow.hoveredNow = hovered
                    }
                    TapHandler {
                        onTapped: {
                            root.selectedIndex = index;
                            root.run(index);
                        }
                    }
                }
            }
        }
    }
}
