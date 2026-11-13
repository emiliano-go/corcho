import QtQuick

Item {
    id: root

    property var app
    property var th
    property var settings
    property var grids: []
    property var folders: []
    property string activeGridId: ""
    property bool collapsed: false
    property bool gridHovered: false

    signal gridSelected(string id)
    signal newGridRequested()
    signal newFolderRequested()
    signal templatesRequested()
    signal settingsRequested()
    signal toggleCollapsed()
    signal gridMenuRequested(var grid, real x, real y)
    signal folderMenuRequested(var folder, real x, real y)

    implicitWidth: collapsed ? root.th.sidebarCollapsedWidth : root.th.sidebarWidth


    Rectangle {
        anchors.fill: parent
        color: root.th.sidebar

        Rectangle {
            anchors.right: parent.right
            width: root.th.borderWidth
            height: parent.height
            color: root.th.border
        }
    }

    Column {
        id: header
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.topMargin: root.th.space4
        spacing: root.th.space2
        Item {
            width: parent.width
            height: root.th.sidebarHeaderHeight

            Row {
                anchors.left: parent.left
                anchors.leftMargin: 20
                anchors.verticalCenter: parent.verticalCenter
                spacing: root.th.space2
                visible: !root.collapsed

                Icon {
                    anchors.verticalCenter: parent.verticalCenter
                    name: "pin"
                    size: root.th.iconLarge
                    color: root.th.accent
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: "Corcho"
                    color: root.th.text
                    font.family: root.th.brandFont
                    font.pixelSize: root.th.fontSizeLarge
                    font.weight: Font.Bold
                }
            }

            Item {
                visible: root.collapsed
                anchors.centerIn: parent
                width: 24
                height: 24
                Icon {
                    anchors.centerIn: parent
                    name: "pin"
                    size: root.th.iconLarge
                    color: root.th.accent
                }
            }
        }

        Row {
            anchors.horizontalCenter: parent.horizontalCenter
            spacing: root.th.space2
            visible: !root.collapsed

            AppButton {
                th: root.th
                icon: "plus"
                text: "Grid"
                onClicked: root.newGridRequested()
            }
            AppButton {
                th: root.th
                icon: "folder-plus"
                text: "Folder"
                onClicked: root.newFolderRequested()
            }
        }
    }

    Column {
        id: footer
        anchors.bottom: parent.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottomMargin: root.th.space4
        spacing: root.th.space1
        Item {
            width: parent.width
            height: root.th.statusbarHeight

            Row {
                anchors.left: parent.left
                anchors.leftMargin: 20
                anchors.verticalCenter: parent.verticalCenter
                spacing: root.th.space2
                visible: !root.collapsed

                Rectangle {
                    width: 8
                    height: 8
                    radius: root.th.radiusMicro
                    color: root.app.syncing ? root.th.accent : (root.settings && root.settings.cloud_sync_enabled ? root.th.success : root.th.muted)
                    anchors.verticalCenter: parent.verticalCenter
                }
                Text {
                    anchors.verticalCenter: parent.verticalCenter
                    text: root.app.syncing ? "Syncing" : (root.settings && root.settings.cloud_sync_enabled ? "Cloud sync on" : "Local only")
                    color: root.th.muted
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
                }
            }
        }

        Item {
            width: parent.width
            height: root.th.controlHeight

            AppButton {
                anchors.left: parent.left
                anchors.leftMargin: root.collapsed ? 4 : 12
                anchors.verticalCenter: parent.verticalCenter
                th: root.th
                icon: "settings"
                text: root.collapsed ? "" : "Settings"
                onClicked: root.settingsRequested()
                width: root.collapsed ? 28 : parent.width - 60
            }

            Item {
                id: collapseToggle
                anchors.right: parent.right
                anchors.rightMargin: root.collapsed ? 4 : 12
                anchors.verticalCenter: parent.verticalCenter
                width: 24
                height: 24
                activeFocusOnTab: true
                Accessible.role: Accessible.Button
                Accessible.name: root.collapsed ? "Expand sidebar" : "Collapse sidebar"
                Accessible.onPressAction: root.toggleCollapsed()
                Keys.onSpacePressed: root.toggleCollapsed()
                Keys.onReturnPressed: root.toggleCollapsed()
                Keys.onEnterPressed: root.toggleCollapsed()

                Rectangle {
                    anchors.fill: parent
                    radius: root.th.radiusControl
                    color: "transparent"
                    border.width: collapseToggle.activeFocus ? root.th.borderWidthFocus : root.th.borderWidth
                    border.color: collapseToggle.activeFocus ? root.th.accent : "transparent"
                }
                Icon {
                    anchors.centerIn: parent
                    name: "panel-left"
                    size: root.th.iconSmall
                    color: root.th.text2
                }
                TapHandler { onTapped: root.toggleCollapsed() }
            }
        }
    }

    Flickable {
        id: list
        anchors.top: header.bottom
        anchors.bottom: footer.top
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.topMargin: 8
        anchors.bottomMargin: 8
        clip: true
        contentHeight: listColumn.height
        boundsBehavior: Flickable.StopAtBounds

        WheelHandler {
            onWheel: function(event) {
                var step = event.pixelDelta.y !== 0 ? event.pixelDelta.y : event.angleDelta.y;
                if (step === 0) return;
                list.cancelFlick();
                list.contentY = Math.max(0, Math.min(list.contentHeight - list.height, list.contentY - step));
                event.accepted = true;
            }
        }

        Column {
            id: listColumn
            width: list.width
            spacing: root.th.spaceHalf
            Item {
                id: templatesRow
                width: listColumn.width
                height: root.th.controlHeight
                property bool hoveredNow: false
                activeFocusOnTab: true
                Accessible.role: Accessible.Button
                Accessible.name: "Templates"
                Accessible.onPressAction: root.templatesRequested()
                Keys.onSpacePressed: root.templatesRequested()
                Keys.onReturnPressed: root.templatesRequested()
                Keys.onEnterPressed: root.templatesRequested()

                Rectangle {
                    anchors.fill: parent
                    anchors.leftMargin: root.th.space2
                    anchors.rightMargin: root.th.space2
                    radius: root.th.radiusControl
                    color: templatesRow.hoveredNow ? root.th.hover : "transparent"
                    border.width: templatesRow.activeFocus ? root.th.borderWidthFocus : 0
                    border.color: root.th.accent
                }

                Row {
                    x: root.collapsed ? (templatesRow.width - width) / 2 : 20
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: root.th.space2

                    Icon {
                        anchors.verticalCenter: parent.verticalCenter
                        name: "layout-template"
                        size: root.th.iconBody
                        color: root.th.text2
                    }
                    Text {
                        visible: !root.collapsed
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Templates"
                        color: root.th.text2
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeBody
                    }
                }

                HoverHandler {
                    onHoveredChanged: templatesRow.hoveredNow = hovered
                }
                TapHandler {
                    acceptedButtons: Qt.LeftButton
                    onTapped: root.templatesRequested()
                }
            }

            Repeater {
                model: root.folders

                delegate: Column {
                    required property var modelData
                    width: listColumn.width

                    Item {
                        id: folderHeader
                        width: parent.width
                        height: root.th.menuItemHeight
                        activeFocusOnTab: true
                        Accessible.role: Accessible.Button
                        Accessible.name: modelData.name + (modelData.collapsed ? ", collapsed folder" : ", expanded folder")
                        Accessible.onPressAction: root.app.toggleFolder(modelData.id)
                        Keys.onSpacePressed: root.app.toggleFolder(modelData.id)
                        Keys.onReturnPressed: root.app.toggleFolder(modelData.id)
                        Keys.onEnterPressed: root.app.toggleFolder(modelData.id)

                        Rectangle {
                            anchors.fill: parent
                            anchors.margins: 2
                            radius: root.th.radiusControl
                            color: "transparent"
                            border.width: folderHeader.activeFocus ? root.th.borderWidthFocus : 0
                            border.color: root.th.accent
                        }

                        Row {
                            anchors.left: parent.left
                            anchors.leftMargin: 20
                            anchors.verticalCenter: parent.verticalCenter
                            spacing: root.th.space2
                            visible: !root.collapsed

                            Icon {
                                anchors.verticalCenter: parent.verticalCenter
                                name: modelData.collapsed ? "chevron-right" : "chevron-down"
                                size: root.th.iconBody
                                color: root.th.muted
                            }
                            Icon {
                                anchors.verticalCenter: parent.verticalCenter
                                name: "folder-plus"
                                size: root.th.iconBody
                                color: root.th.text2
                            }
                            Text {
                                anchors.verticalCenter: parent.verticalCenter
                                text: modelData.name
                                color: root.th.text2
                                font.family: root.th.fontFamily
                                font.pixelSize: root.th.fontSizeSmall
                                font.weight: root.th.fontWeightMedium
                            }
                        }

                        TapHandler {
                            acceptedButtons: Qt.LeftButton
                            onTapped: root.app.toggleFolder(modelData.id)
                        }
                        TapHandler {
                            acceptedButtons: Qt.RightButton
                            onTapped: {
                                var p = mapToItem(null, width - 20, height);
                                root.folderMenuRequested(modelData, p.x, p.y);
                            }
                        }
                    }

                    Repeater {
                        model: root.grids.filter(function (g) { return g.folderId === modelData.id; })
                        delegate: gridRow
                    }
                }
            }

            Repeater {
                model: root.grids.filter(function (g) { return !g.folderId; })
                delegate: gridRow
            }

            DropArea {
                width: listColumn.width
                height: 28
                onDropped: function (drop) {
                    var id = drop.source && drop.source.modelData
                        ? drop.source.modelData.id
                        : drop.getDataAsString("text/plain");
                    if (id)
                        root.app.reorderGrid(id, "");
                }
            }
        }
    }

    Component {
        id: gridRow

        Item {
            id: row
            required property var modelData
            property bool hoveredNow: false
            property bool dropTarget: false

            width: listColumn.width
            height: root.th.controlHeight
            activeFocusOnTab: true

            Drag.active: gridDrag.active
            Drag.source: row
            Drag.dragType: Drag.Internal
            Drag.mimeData: { "text/plain": modelData.id }
            Drag.hotSpot.x: row.width / 2
            Drag.hotSpot.y: row.height / 2
            Accessible.role: Accessible.Button
            Accessible.name: modelData.name + ", " + modelData.cards.length + " cards"
            Accessible.selected: root.activeGridId === modelData.id
            Accessible.onPressAction: root.gridSelected(row.modelData.id)
            Keys.onSpacePressed: root.gridSelected(row.modelData.id)
            Keys.onReturnPressed: root.gridSelected(row.modelData.id)
            Keys.onEnterPressed: root.gridSelected(row.modelData.id)

            Rectangle {
                anchors.fill: parent
                anchors.leftMargin: root.th.space2
                anchors.rightMargin: root.th.space2
                radius: root.th.radiusControl
                color: root.activeGridId === modelData.id ? root.th.surfaceStrong : (row.hoveredNow ? root.th.hover : "transparent")
                border.width: row.activeFocus ? root.th.borderWidthFocus : 0
                border.color: root.th.accent
            }

            Row {
                x: root.collapsed ? (row.width - width) / 2 : 20
                anchors.verticalCenter: parent.verticalCenter
                spacing: root.th.space2
                GridIcon {
                    anchors.verticalCenter: parent.verticalCenter
                    icon: modelData.icon || "layout-grid"
                    size: root.th.iconBody
                    color: root.activeGridId === modelData.id ? root.th.accent : root.th.text2
                }
                Text {
                    visible: !root.collapsed
                    anchors.verticalCenter: parent.verticalCenter
                    width: row.width - 100
                    text: modelData.name
                    color: root.activeGridId === modelData.id ? root.th.text : root.th.text2
                    elide: Text.ElideRight
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeBody
                    font.weight: root.activeGridId === modelData.id ? 500 : 400
                }
                Text {
                    visible: !root.collapsed
                    anchors.verticalCenter: parent.verticalCenter
                    text: modelData.cards.length
                    color: root.th.muted
                    font.family: root.th.fontMono
                    font.pixelSize: root.th.fontSizeSmall
                }
            }

            HoverHandler {
                onHoveredChanged: row.hoveredNow = hovered
            }
            TapHandler {
                acceptedButtons: Qt.LeftButton
                onTapped: root.gridSelected(row.modelData.id)
            }
            TapHandler {
                acceptedButtons: Qt.RightButton
                onTapped: {
                    var p = mapToItem(null, row.width - 20, row.height);
                    root.gridMenuRequested(row.modelData, p.x, p.y);
                }
            }

            DragHandler {
                id: gridDrag
                target: null
                dragThreshold: 6
            }

            Rectangle {
                anchors.top: parent.top
                anchors.left: parent.left
                anchors.right: parent.right
                height: 2
                visible: row.dropTarget
                color: root.th.accent
            }

            DropArea {
                anchors.fill: parent
                onEntered: row.dropTarget = true
                onExited: row.dropTarget = false
                onDropped: function (drop) {
                    row.dropTarget = false;
                    var id = drop.source && drop.source.modelData
                        ? drop.source.modelData.id
                        : drop.getDataAsString("text/plain");
                    if (id && id !== modelData.id)
                        root.app.reorderGrid(id, modelData.id);
                }
            }
        }
    }
}
