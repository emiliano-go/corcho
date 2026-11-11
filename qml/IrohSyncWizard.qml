import QtQuick

Item {
    id: root

    property var app
    property var th
    property bool opened: false
    property string mode: "home" // home | join | share
    property string joinText: ""
    property string errorText: ""
    property string note: ""
    property string setupCode: ""
    property string shownPairCode: ""
    property bool busy: false
    property bool advanced: false
    property string relayText: ""
    property string relayError: ""
    property string relaySaved: ""
    property string accessError: ""
    property string accessSaved: ""

    signal closed()

    anchors.fill: parent
    visible: opened
    z: 640

    readonly property var status: {
        try { return JSON.parse(app.irohJson || "{}"); } catch (e) { return {}; }
    }
    readonly property bool configured: root.status.savedWorkspace === true
        || (root.status.namespace !== null && root.status.namespace !== undefined)
    readonly property bool encrypted: root.status.encrypted === true

    Timer {
        interval: 2000
        repeat: true
        running: root.opened
        onTriggered: root.app.refresh()
    }

    function open() {
        mode = "home";
        joinText = "";
        errorText = "";
        note = "";
        setupCode = "";
        shownPairCode = "";
        busy = false;
        advanced = false;
        relayText = root.app.irohRelayUrl();
        relayError = "";
        relaySaved = "";
        accessError = "";
        accessSaved = "";
        opened = true;
    }

    function close() {
        opened = false;
        root.closed();
    }

    function setSetting(key, value) {
        root.app.setSetting(key, JSON.stringify(value));
    }

    function looksLikeWords(text) {
        var parts = text.trim().toLowerCase().split(/[^a-z]+/);
        var words = 0;
        for (var i = 0; i < parts.length; i++) {
            if (parts[i].length > 0)
                words++;
        }
        return words === 6;
    }

    function applyRelay() {
        var next = root.relayText.trim();
        if (next === root.app.irohRelayUrl())
            return true;
        var error = root.app.irohSetRelay(next);
        if (error !== "") {
            errorText = error;
            return false;
        }
        return true;
    }

    function createNetwork() {
        errorText = "";
        note = "";
        if (!root.applyRelay())
            return;
        busy = true;
        var ticket = root.app.irohCreate();
        busy = false;
        if (ticket === "") {
            errorText = "Could not create a network.";
            return;
        }
        root.setSetting("iroh_sync_enabled", true);
        root.app.refresh();
        mode = "share";
    }

    function joinNetwork() {
        errorText = "";
        note = "";
        var value = root.joinText.trim();
        if (value === "") {
            errorText = "Enter the six words or the setup code.";
            return;
        }
        if (!root.applyRelay())
            return;
        busy = true;
        if (looksLikeWords(value)) {
            var error = root.app.irohPairJoin(value);
            busy = false;
            if (error !== "") {
                errorText = error;
                return;
            }
        } else {
            var namespace = root.app.irohJoin(value);
            busy = false;
            if (namespace === "") {
                errorText = "Could not join with that setup code.";
                return;
            }
        }
        root.setSetting("iroh_sync_enabled", true);
        root.app.refresh();
        mode = "share";
    }

    function copyWords() {
        errorText = "";
        note = "";
        shownPairCode = "";
        var code = root.app.irohPairCreate();
        if (code === "") {
            errorText = "Could not create a six-word code. Check the relay in Advanced.";
            return;
        }
        shownPairCode = code;
        root.app.copyText(code);
        note = "Six-word code copied. It expires in 10 minutes and works once.";
    }

    function copySetup() {
        errorText = "";
        note = "";
        setupCode = "";
        var code = root.app.irohShare(false);
        if (code === "") {
            errorText = "Could not create a setup code. Create or join a network first.";
            return;
        }
        setupCode = code;
        root.app.copyText(code);
        note = "Setup code copied. Treat it like a password.";
    }

    function saveRelay() {
        relayError = "";
        relaySaved = "";
        var value = root.app.irohSetRelay(relayText.trim());
        if (value !== "") {
            relayError = value;
            return;
        }
        relaySaved = "Relay saved.";
        root.app.refresh();
    }

    function requestAccess() {
        accessError = "";
        accessSaved = "";
        var base = relayText.trim().replace(/\/+$/, "");
        var id = root.status.endpointId || "";
        if (base === "") {
            accessError = "Set a relay endpoint first.";
            return;
        }
        if (id === "") {
            accessError = "The device ID is not ready yet.";
            return;
        }
        var xhr = new XMLHttpRequest();
        xhr.open("POST", base + "/access/request", true);
        xhr.setRequestHeader("Content-Type", "application/json");
        xhr.onreadystatechange = function () {
            if (xhr.readyState !== XMLHttpRequest.DONE)
                return;
            if (xhr.status === 200 || xhr.status === 202) {
                var state = "pending";
                try { state = JSON.parse(xhr.responseText).status || state; } catch (e) {}
                accessSaved = state === "approved"
                    ? "This device is already approved."
                    : "Access requested. The relay operator must approve this device.";
            } else {
                accessError = "Request failed (" + xhr.status + ").";
            }
        };
        xhr.send(JSON.stringify({ endpointId: id }));
    }

    function checkAccess() {
        accessError = "";
        accessSaved = "";
        var base = relayText.trim().replace(/\/+$/, "");
        var id = root.status.endpointId || "";
        if (base === "" || id === "") {
            accessError = "Set a relay endpoint first.";
            return;
        }
        var xhr = new XMLHttpRequest();
        xhr.open("GET", base + "/access/status?id=" + encodeURIComponent(id), true);
        xhr.onreadystatechange = function () {
            if (xhr.readyState !== XMLHttpRequest.DONE)
                return;
            if (xhr.status !== 200) {
                accessError = "Could not check access (" + xhr.status + ").";
                return;
            }
            var state = "unknown";
            try { state = JSON.parse(xhr.responseText).status || state; } catch (e) {}
            if (state === "approved")
                accessSaved = "This device is approved.";
            else if (state === "pending")
                accessSaved = "Waiting for the relay operator.";
            else
                accessError = "No access request found for this device.";
        };
        xhr.send();
    }

    function copyCacheTicket() {
        errorText = "";
        note = "";
        var value = root.app.irohCacheTicket();
        if (value === "") {
            errorText = "No cache ticket is available.";
            return;
        }
        root.app.copyText(value);
        note = "Cache ticket copied (read-only, no encryption key).";
    }

    function leaveNetwork() {
        root.app.irohLeave();
        root.setSetting("iroh_sync_enabled", false);
        root.app.refresh();
        mode = "home";
        note = "Left the network.";
    }

    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.55)
        MouseArea {
            anchors.fill: parent
            onClicked: function (mouse) {
                var p = panel.mapFromItem(root, mouse.x, mouse.y);
                if (p.x < 0 || p.y < 0 || p.x > panel.width || p.y > panel.height)
                    root.close();
            }
        }
    }

    Rectangle {
        id: panel
        width: Math.min(640, root.width - 32)
        height: Math.min(680, root.height - 32)
        anchors.centerIn: parent
        radius: root.th.radiusPanel
        color: root.th.menu
        border.width: root.th.borderWidth
        border.color: root.th.border

        MouseArea { anchors.fill: parent }

        Flickable {
            id: flick
            anchors.fill: parent
            anchors.margins: root.th.space5
            contentWidth: width
            contentHeight: body.implicitHeight
            clip: true
            boundsBehavior: Flickable.StopAtBounds

            Column {
                id: body
                width: flick.width
                spacing: root.th.space4

                Item {
                    width: parent.width
                    height: 26
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        width: parent.width - 28
                        text: "Peer sync"
                        color: root.th.text
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizePage
                        font.weight: root.th.fontWeightMedium
                    }
                    Icon {
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        name: "x"
                        size: root.th.iconBody
                        color: root.th.muted
                        TapHandler { onTapped: root.close() }
                    }
                }

                Text {
                    width: parent.width
                    text: root.status.lastError
                        ? ("Error: " + root.status.lastError)
                        : (root.configured
                            ? (root.status.peers > 0
                                ? ("Connected to " + root.status.peers + " device(s).")
                                : "Network ready. Waiting for devices.")
                            : "No network on this device.")
                    color: root.status.lastError ? root.th.danger : root.th.muted
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
                }

                Column {
                    width: parent.width
                    spacing: root.th.space2
                    visible: root.mode === "home"

                    Text {
                        width: parent.width
                        text: root.configured
                            ? "Invite another device, or join a different network."
                            : "Create a network for your devices, or join one with an invite."
                        color: root.th.text2
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeBody
                    }

                    AppButton {
                        width: parent.width
                        th: root.th
                        text: root.configured ? "Invite a device" : "Create a network"
                        variant: "solid"
                        onClicked: {
                            if (root.configured)
                                root.mode = "share";
                            else
                                root.mode = "create";
                        }
                    }
                    AppButton {
                        width: parent.width
                        th: root.th
                        text: root.configured ? "Join a different network" : "Join a network"
                        onClicked: root.mode = "join"
                    }
                }

                Column {
                    width: parent.width
                    spacing: root.th.space2
                    visible: root.mode === "create"

                    Text {
                        width: parent.width
                        text: "Create a network"
                        color: root.th.text
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeBody
                        font.weight: root.th.fontWeightMedium
                    }
                    Text {
                        width: parent.width
                        text: "Every device in this network must use the same relay."
                        color: root.th.text2
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Text {
                        text: "Relay endpoint"
                        color: root.th.text2
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    AppTextField {
                        width: parent.width
                        th: root.th
                        text: root.relayText
                        placeholder: "https://relay.example.com"
                        onEdited: function (value) { root.relayText = value; }
                        onAccepted: root.createNetwork()
                    }
                    Text {
                        width: parent.width
                        text: "Leave empty for the public n0 relays."
                        color: root.th.muted
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeCaption
                    }
                    Row {
                        spacing: root.th.space2
                        AppButton {
                            th: root.th
                            text: root.busy ? "Creating…" : "Create"
                            variant: "solid"
                            enabledButton: !root.busy
                            onClicked: root.createNetwork()
                        }
                        AppButton {
                            th: root.th
                            text: "Back"
                            onClicked: root.mode = "home"
                        }
                    }
                }

                Column {
                    width: parent.width
                    spacing: root.th.space2
                    visible: root.mode === "join"

                    Text {
                        width: parent.width
                        text: "Enter the six words from the other device, or paste its setup code."
                        color: root.th.text2
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeBody
                    }

                    Text {
                        text: "Relay endpoint"
                        color: root.th.text2
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    AppTextField {
                        width: parent.width
                        th: root.th
                        text: root.relayText
                        placeholder: "https://relay.example.com"
                        onEdited: function (value) { root.relayText = value; }
                    }

                    Rectangle {
                        width: parent.width
                        height: 82
                        radius: root.th.radiusControl
                        color: root.th.input
                        border.width: root.th.borderWidth
                        border.color: joinInput.activeFocus ? root.th.accent : root.th.border

                        TextEdit {
                            id: joinInput
                            anchors.fill: parent
                            anchors.margins: root.th.space2
                            text: root.joinText
                            color: root.th.text
                            selectionColor: root.th.accent
                            selectedTextColor: root.th.accentInk
                            font.family: root.th.fontMono
                            font.pixelSize: root.th.fontSizeSmall
                            wrapMode: TextEdit.WrapAnywhere
                            selectByMouse: true
                            onTextChanged: root.joinText = text
                        }
                        Text {
                            anchors.fill: parent
                            anchors.margins: root.th.space2
                            visible: root.joinText.length === 0
                            text: "Six words, or paste the setup code"
                            color: root.th.muted
                            wrapMode: Text.WordWrap
                            font.family: root.th.fontFamily
                            font.pixelSize: root.th.fontSizeSmall
                        }
                    }

                    Row {
                        spacing: root.th.space2
                        AppButton {
                            th: root.th
                            text: root.busy ? "Joining…" : "Join"
                            variant: "solid"
                            enabledButton: !root.busy
                            onClicked: root.joinNetwork()
                        }
                        AppButton {
                            th: root.th
                            text: "Back"
                            onClicked: root.mode = "home"
                        }
                    }
                }

                Column {
                    width: parent.width
                    spacing: root.th.space2
                    visible: root.mode === "share"

                    Text {
                        width: parent.width
                        text: "Invite another device"
                        color: root.th.text
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeBody
                        font.weight: root.th.fontWeightMedium
                    }
                    Text {
                        width: parent.width
                        text: "The six-word code is easiest. Both codes contain the encryption key; treat them like passwords."
                        color: root.th.text2
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }

                    AppButton {
                        width: parent.width
                        th: root.th
                        text: "Copy six-word code"
                        variant: "solid"
                        onClicked: root.copyWords()
                    }
                    TextEdit {
                        width: parent.width
                        height: 32
                        visible: root.shownPairCode !== ""
                        text: root.shownPairCode
                        readOnly: true
                        selectByMouse: true
                        wrapMode: TextEdit.WrapAnywhere
                        clip: true
                        color: root.th.text
                        font.family: root.th.fontMono
                        font.pixelSize: root.th.fontSizeBody
                    }
                    Row {
                        spacing: root.th.space2
                        visible: root.shownPairCode !== ""
                        AppButton {
                            th: root.th
                            text: "Copy again"
                            onClicked: root.app.copyText(root.shownPairCode)
                        }
                    }

                    AppButton {
                        width: parent.width
                        th: root.th
                        text: "Copy setup code"
                        onClicked: root.copySetup()
                    }
                    TextEdit {
                        width: parent.width
                        height: 64
                        visible: root.setupCode !== ""
                        text: root.setupCode
                        readOnly: true
                        selectByMouse: true
                        wrapMode: TextEdit.WrapAnywhere
                        clip: true
                        color: root.th.text
                        font.family: root.th.fontMono
                        font.pixelSize: root.th.fontSizeCaption
                        padding: root.th.space2
                    }
                    Row {
                        spacing: root.th.space2
                        visible: root.setupCode !== ""
                        AppButton {
                            th: root.th
                            text: "Copy again"
                            onClicked: root.app.copyText(root.setupCode)
                        }
                    }

                    Row {
                        spacing: root.th.space2
                        AppButton {
                            th: root.th
                            text: "Done"
                            onClicked: {
                                root.setupCode = "";
                                root.shownPairCode = "";
                                root.note = "";
                                root.mode = "home";
                            }
                        }
                    }
                }

                Text {
                    width: parent.width
                    text: root.errorText
                    visible: root.errorText !== ""
                    color: root.th.danger
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
                }
                Text {
                    width: parent.width
                    text: root.note
                    visible: root.note !== ""
                    color: root.th.success
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
                }

                Rectangle {
                    width: parent.width
                    height: root.th.borderWidth
                    color: root.th.border
                }

                AppButton {
                    th: root.th
                    text: root.advanced ? "Hide advanced" : "Advanced"
                    onClicked: root.advanced = !root.advanced
                }

                Column {
                    width: parent.width
                    spacing: root.th.space2
                    visible: root.advanced

                    Text {
                        text: "Relay endpoint"
                        color: root.th.text2
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Row {
                        width: parent.width
                        spacing: root.th.space2
                        AppTextField {
                            width: parent.width - saveRelay.width - root.th.space2
                            th: root.th
                            text: root.relayText
                            placeholder: "https://relay.example.com"
                            onEdited: function (value) { root.relayText = value; }
                            onAccepted: root.saveRelay()
                        }
                        AppButton {
                            id: saveRelay
                            th: root.th
                            text: "Save"
                            onClicked: root.saveRelay()
                        }
                    }
                    Text {
                        width: parent.width
                        text: "Leave empty for the public n0 relays."
                        color: root.th.muted
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeCaption
                    }
                    Text {
                        width: parent.width
                        text: root.relayError
                        visible: root.relayError !== ""
                        color: root.th.danger
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Text {
                        width: parent.width
                        text: root.relaySaved
                        visible: root.relaySaved !== ""
                        color: root.th.success
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }

                    Text {
                        text: "This device ID"
                        color: root.th.text2
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Row {
                        width: parent.width
                        spacing: root.th.space2
                        TextEdit {
                            width: parent.width - copyDeviceId.width - root.th.space2
                            height: 34
                            text: root.status.endpointId || ""
                            readOnly: true
                            selectByMouse: true
                            wrapMode: TextEdit.WrapAnywhere
                            clip: true
                            color: root.th.text
                            font.family: root.th.fontMono
                            font.pixelSize: root.th.fontSizeCaption
                        }
                        AppButton {
                            id: copyDeviceId
                            th: root.th
                            text: "Copy"
                            onClicked: root.app.copyText(root.status.endpointId)
                        }
                    }
                    Row {
                        spacing: root.th.space2
                        AppButton {
                            th: root.th
                            text: "Request access"
                            onClicked: root.requestAccess()
                        }
                        AppButton {
                            th: root.th
                            text: "Check access"
                            onClicked: root.checkAccess()
                        }
                    }
                    Text {
                        width: parent.width
                        text: root.accessError
                        visible: root.accessError !== ""
                        color: root.th.danger
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Text {
                        width: parent.width
                        text: root.accessSaved
                        visible: root.accessSaved !== ""
                        color: root.th.success
                        wrapMode: Text.WordWrap
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }

                    Row {
                        spacing: root.th.space2
                        AppButton {
                            th: root.th
                            text: "Copy cache ticket"
                            onClicked: root.copyCacheTicket()
                        }
                        AppButton {
                            th: root.th
                            text: "Sync now"
                            enabledButton: root.configured
                            onClicked: root.app.irohSyncNow()
                        }
                    }
                    AppButton {
                        th: root.th
                        text: "Leave network"
                        variant: "danger"
                        enabledButton: root.configured
                        onClicked: root.leaveNetwork()
                    }
                }

                Text {
                    width: parent.width
                    text: (root.encrypted ? "Encryption: on" : "Encryption: off")
                        + (root.status.fingerprint
                            ? "   Fingerprint: " + root.status.fingerprint.slice(0, 16)
                            : "")
                    color: root.encrypted ? root.th.success : root.th.muted
                    wrapMode: Text.WrapAnywhere
                    font.family: root.th.fontMono
                    font.pixelSize: root.th.fontSizeCaption
                }
            }
        }
    }
}
