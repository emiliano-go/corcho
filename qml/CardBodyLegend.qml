import QtQuick

CardFrame {
    id: root

    readonly property var swatches: ["#86b78a", "#7aa2f7", "#d97a72", "#c9a86a", "#8d8d97"]
    readonly property var entries: (root.card && root.card.entries) ? root.card.entries : []
    readonly property string active: (root.card && root.card.active) ? root.card.active : ""

    function persist(nextEntries, nextActive) {
        root.commit({ entries: nextEntries, active: nextActive });
    }

    function updateEntry(index, patch) {
        var next = JSON.parse(JSON.stringify(entries));
        for (var key in patch) next[index][key] = patch[key];
        persist(next, active);
    }

    function cycleColor(index) {
        var entry = entries[index];
        var at = swatches.indexOf(entry.color);
        updateEntry(index, { color: swatches[(at + 1) % swatches.length] });
    }

    function addEntry() {
        var next = JSON.parse(JSON.stringify(entries));
        var entry = {
            id: "l" + Date.now(),
            label: "New meaning",
            color: swatches[next.length % swatches.length],
            dashed: false
        };
        next.push(entry);
        persist(next, active || entry.id);
    }

    function removeEntry(index) {
        var next = JSON.parse(JSON.stringify(entries));
        var removed = next.splice(index, 1)[0];
        persist(next, active === (removed && removed.id) ? (next[0] ? next[0].id : "") : active);
    }

    Column {
        anchors.fill: parent
        spacing: root.th.spaceSnug

        Row {
            width: parent.width
            spacing: root.th.space2

            Text {
                anchors.verticalCenter: parent.verticalCenter
                text: "Arrow legend"
                color: root.th.text
                font.family: root.th.fontFamily
                font.pixelSize: root.th.fontSizeBody
                font.weight: root.th.fontWeightMedium
            }

            Item { width: Math.max(0, parent.width - 180); height: 1 }

            AppButton {
                th: root.th
                icon: "plus"
                text: "Add"
                onClicked: root.addEntry()
            }
        }

        Text {
            width: parent.width
            text: "Click the dot to make a meaning active. New arrows use the active colour and style."
            color: root.th.muted
            wrapMode: Text.WordWrap
            font.family: root.th.fontFamily
            font.pixelSize: root.th.fontSizeSmall
        }

        Repeater {
            model: root.entries

            delegate: Rectangle {
                required property var modelData
                required property int index

                width: parent.width
                height: 36
                radius: root.th.radiusControl
                color: root.active === modelData.id
                    ? Qt.alpha(root.th.accent, 0.12)
                    : "transparent"
                border.width: root.active === modelData.id ? 1 : 0
                border.color: Qt.alpha(root.th.accent, 0.5)

                Row {
                    anchors.fill: parent
                    anchors.leftMargin: 6
                    anchors.rightMargin: 4
                    spacing: root.th.spaceSnug

                    Item {
                        width: 22
                        height: 22
                        anchors.verticalCenter: parent.verticalCenter
                        Rectangle {
                            anchors.centerIn: parent
                            width: 18
                            height: 18
                            radius: 9
                            color: "transparent"
                            border.width: 1
                            border.color: root.active === modelData.id
                                ? root.th.accent
                                : root.th.borderStrong
                        }
                        Rectangle {
                            anchors.centerIn: parent
                            visible: root.active === modelData.id
                            width: 10
                            height: 10
                            radius: 5
                            color: root.th.accent
                        }
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.persist(root.entries, modelData.id)
                        }
                    }

                    Rectangle {
                        width: 22
                        height: 22
                        radius: 6
                        anchors.verticalCenter: parent.verticalCenter
                        color: modelData.color || root.th.accent
                        border.width: 1
                        border.color: Qt.alpha(root.th.text, 0.25)
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.cycleColor(index)
                        }
                    }

                    TextInput {
                        width: parent.width - 150
                        height: 26
                        anchors.verticalCenter: parent.verticalCenter
                        text: modelData.label || ""
                        color: root.th.text
                        selectionColor: root.th.accent
                        selectedTextColor: root.th.accentInk
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                        verticalAlignment: TextInput.AlignVCenter
                        clip: true
                        onEditingFinished: {
                            if (text.trim() !== (modelData.label || ""))
                                root.updateEntry(index, { label: text.trim() });
                        }
                    }

                    Rectangle {
                        width: 52
                        height: 22
                        radius: root.th.radiusDot
                        anchors.verticalCenter: parent.verticalCenter
                        color: modelData.dashed ? Qt.alpha(root.th.accent, 0.18) : "transparent"
                        border.width: 1
                        border.color: modelData.dashed ? root.th.accent : root.th.borderStrong
                        Text {
                            anchors.centerIn: parent
                            text: "dash"
                            color: modelData.dashed ? root.th.text : root.th.muted
                            font.family: root.th.fontFamily
                            font.pixelSize: root.th.fontSizeSmall
                        }
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.updateEntry(index, { dashed: !modelData.dashed })
                        }
                    }

                    Item {
                        width: 22
                        height: 22
                        anchors.verticalCenter: parent.verticalCenter
                        Text {
                            anchors.centerIn: parent
                            text: "×"
                            color: root.th.muted
                            font.pixelSize: 18
                        }
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.removeEntry(index)
                        }
                    }
                }
            }
        }
    }
}
