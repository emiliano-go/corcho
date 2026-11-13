import QtQuick

CardFrame {
    id: root

    readonly property var links: {
        try { return (JSON.parse(root.app.calendarJson).dateLinks || []); }
        catch (e) { return []; }
    }

    function linkFor(itemId) {
        for (var i = 0; i < links.length; i++)
            if (links[i].sourceCardId === root.card.id && links[i].sourceItemId === itemId)
                return links[i];
        return null;
    }

    function shortDate(key) {
        var p = key.split("-");
        if (p.length !== 3) return key;
        var d = new Date(Number(p[0]), Number(p[1]) - 1, Number(p[2]));
        return d.toLocaleDateString(Qt.locale(), "MMM d");
    }

    property var columns: {
        var c = card.columns ? card.columns : [];
        if (c.length === 0) {
            c = [
                { id: "todo", title: "To do", items: [] },
                { id: "doing", title: "In progress", items: [] },
                { id: "done", title: "Done", items: [] }
            ];
        }
        return c;
    }

    function persist(next) {
        columns = next;
        root.commit({ columns: next });
    }

    function addItem(colIndex, text) {
        if (text.trim() === "") return;
        var next = JSON.parse(JSON.stringify(columns));
        next[colIndex].items.push({ id: "k" + Date.now() + Math.floor(Math.random() * 999), text: text.trim() });
        persist(next);
    }

    function moveTo(colIndex, itemIndex, targetCol) {
        if (colIndex < 0 || colIndex >= columns.length) return;
        if (itemIndex < 0 || itemIndex >= columns[colIndex].items.length) return;
        if (targetCol === colIndex || targetCol < 0 || targetCol >= columns.length) return;
        var next = JSON.parse(JSON.stringify(columns));
        var item = next[colIndex].items.splice(itemIndex, 1)[0];
        next[targetCol].items.push(item);
        persist(next);
    }

    function move(colIndex, itemIndex, dir) {
        if (colIndex < 0 || colIndex >= columns.length) return;
        if (itemIndex < 0 || itemIndex >= columns[colIndex].items.length) return;
        var next = JSON.parse(JSON.stringify(columns));
        var target = colIndex + dir;
        if (target < 0 || target >= next.length) return;
        var item = next[colIndex].items.splice(itemIndex, 1)[0];
        next[target].items.push(item);
        persist(next);
    }

    function remove(colIndex, itemIndex) {
        if (colIndex < 0 || colIndex >= columns.length) return;
        if (itemIndex < 0 || itemIndex >= columns[colIndex].items.length) return;
        var next = JSON.parse(JSON.stringify(columns));
        next[colIndex].items.splice(itemIndex, 1);
        persist(next);
    }

    Row {
        id: columnsRow
        anchors.fill: parent
        spacing: root.th.spaceSnug
        Repeater {
            model: root.columns

            delegate: Item {
                id: column
                required property var modelData
                required property int index

                width: (parent.width - (root.th && root.th.spaceSnug !== undefined ? root.th.spaceSnug : 10) * Math.max(0, root.columns.length - 1))
                       / Math.max(1, root.columns.length)
                height: parent.height

                Rectangle {
                    anchors.fill: parent
                    radius: root.th.radiusMenu
                    color: root.th.surface
                    border.width: root.th.borderWidth
                    border.color: root.th.border
                }

                Column {
                    anchors.fill: parent
                    anchors.margins: 8
                    spacing: root.th.space2
                    Text {
                        text: modelData.title
                        color: root.th.text2
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                        font.weight: root.th.fontWeightMedium
                    }

                    ListView {
                        id: columnList
                        width: parent.width
                        height: parent.height - 48
                        clip: true
                        spacing: root.th.space1
                        model: modelData.items

                        delegate: Item {
                            id: kanbanItem
                            required property var modelData
                            required property int index
                            property bool hoveredNow: false
                            readonly property var link: root.linkFor(modelData.id)

                            width: columnList.width
                            height: 34

                            Rectangle {
                                anchors.fill: parent
                                radius: root.th.radiusKeycap
                                color: kanbanItem.hoveredNow ? root.th.hover : root.th.surfaceStrong
                            }

                            Text {
                                anchors.left: parent.left
                                anchors.leftMargin: 6
                                anchors.right: parent.right
                                anchors.rightMargin: 6
                                anchors.verticalCenter: parent.verticalCenter
                                text: modelData.text
                                color: root.th.text
                                font.family: root.th.fontFamily
                                font.pixelSize: root.th.fontSizeSmall
                                elide: Text.ElideRight
                                wrapMode: Text.Wrap
                                maximumLineCount: 2
                            }

                            Row {
                                anchors.right: parent.right
                                anchors.rightMargin: 4
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: root.th.space1
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    visible: kanbanItem.link !== null
                                    text: kanbanItem.link ? root.shortDate(kanbanItem.link.date) : ""
                                    color: root.th.accent
                                    font.family: root.th.fontFamily
                                    font.pixelSize: root.th.fontSizeMicro
                                }
                                Icon {
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: "calendar"
                                    size: root.th.iconSmall
                                    color: kanbanItem.link !== null ? root.th.accent : root.th.muted
                                    opacity: (kanbanItem.link !== null || kanbanItem.hoveredNow) ? 1 : 0
                                    CardTapHandler {
                                        onAction: root.dateLinkRequested(
                                            root.card.id,
                                            modelData.id,
                                            kanbanItem.link ? kanbanItem.link.id : "",
                                            kanbanItem.link ? kanbanItem.link.date : "")
                                    }
                                }
                                Icon {
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: "chevron-left"
                                    size: root.th.iconSmall
                                    color: root.th.muted
                                    opacity: kanbanItem.hoveredNow ? 1 : 0
                                    CardTapHandler { onAction: root.move(column.index, index, -1) }
                                }
                                Icon {
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: "chevron-right"
                                    size: root.th.iconSmall
                                    color: root.th.muted
                                    opacity: kanbanItem.hoveredNow ? 1 : 0
                                    CardTapHandler { onAction: root.move(column.index, index, 1) }
                                }
                                Icon {
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: "x"
                                    size: root.th.iconSmall
                                    color: root.th.muted
                                    opacity: kanbanItem.hoveredNow ? 1 : 0
                                    CardTapHandler { onAction: root.remove(column.index, index) }
                                }
                            }

                            HoverHandler { id: itemHover
                                onHoveredChanged: kanbanItem.hoveredNow = hovered
                            }
                            CardTapHandler {
                                suppressSelection: false
                                acceptedButtons: Qt.LeftButton
                                onAction: root.requestBodySelect(point.modifiers, point.scenePosition.x, point.scenePosition.y)
                            }
                        }
                    }

                    TextInput {
                        id: addInput
                        width: parent.width
                        height: 24
                        color: root.th.text
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                        clip: true
                        onAccepted: {
                            root.addItem(column.index, text);
                            text = "";
                        }
                        CardTapHandler {
                            suppressSelection: false
                            acceptedButtons: Qt.LeftButton
                            onAction: root.requestBodySelect(point.modifiers, point.scenePosition.x, point.scenePosition.y)
                        }
                        Text {
                            anchors.fill: parent
                            verticalAlignment: Text.AlignVCenter
                            visible: addInput.text.length === 0
                            text: "+ Add"
                            color: root.th.muted
                            font: addInput.font
                        }
                    }
                }
            }
        }
    }

}
