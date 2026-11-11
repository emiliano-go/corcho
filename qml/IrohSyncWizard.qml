import QtQuick

Item {
    id: root

    property var app
    property var th
    property bool opened: false
    property string mode: "home" // home | create | join
    property string ticket: ""
    property string shownTicket: ""
    property string errorText: ""
    property string relayText: ""
    property string relayError: ""
    property string relaySaved: ""
    property string accessError: ""
    property string accessSaved: ""
    property string pairCode: ""
    property string shownPairCode: ""
    property string pairError: ""

    signal closed()

    anchors.fill: parent
    visible: opened
    z: 640

    readonly property var status: {
        try { return JSON.parse(app.irohJson || "{}"); } catch (e) { return {}; }
    }
    readonly property bool hasWorkspace: root.status.namespace !== null
        && root.status.namespace !== undefined

    Timer {
        interval: 2000
        repeat: true
        running: root.opened
        onTriggered: root.app.refresh()
    }

    function open() {
        mode = "home";
        ticket = "";
        shownTicket = "";
        errorText = "";
        relayText = root.app.irohRelayUrl();
        relayError = "";
        relaySaved = "";
        accessError = "";
        accessSaved = "";
        pairCode = "";
        shownPairCode = "";
        pairError = "";
        opened = true;
    }

    function close() {
        opened = false;
        root.closed();
    }

    function setSetting(key, value) {
        root.app.setSetting(key, JSON.stringify(value));
    }

    function create() {
        errorText = "";
        var value = root.app.irohCreate();
        if (value === "") {
            errorText = "Could not create the shared workspace.";
            return;
        }
        shownTicket = value;
        setSetting("iroh_sync_enabled", true);
        mode = "create";
    }

    function join() {
        errorText = "";
        if (ticket.trim() === "") {
            errorText = "Paste a ticket first.";
            return;
        }
        var value = root.app.irohJoin(ticket.trim());
        if (value === "") {
            errorText = "Could not join with that ticket.";
            return;
        }
        setSetting("iroh_sync_enabled", true);
        close();
    }

    function showTicket(readOnly) {
        errorText = "";
        var value = root.app.irohShare(readOnly);
        if (value === "") {
            errorText = readOnly ? "No read ticket is available." : "No write ticket is available.";
            return;
        }
        shownTicket = value;
    }

    function createPair() {
        pairError = "";
        shownPairCode = "";
        var code = root.app.irohPairCreate();
        if (code === "") {
            pairError = "Could not create a word code.";
            return;
        }
        shownPairCode = code;
    }

    function joinPair() {
        pairError = "";
        if (root.pairCode.trim() === "") {
            pairError = "Enter the word code.";
            return;
        }
        var error = root.app.irohPairJoin(root.pairCode.trim());
        if (error !== "") {
            pairError = error;
            return;
        }
        root.setSetting("iroh_sync_enabled", true);
        root.close();
    }

    function copyCacheTicket() {
        errorText = "";
        var value = root.app.irohCacheTicket();
        if (value === "") {
            errorText = "No cache ticket is available.";
            return;
        }
        root.app.copyText(value);
    }

    function saveRelay() {
        relayError = "";
        relaySaved = "";
        var value = root.app.irohSetRelay(relayText.trim());
        if (value !== "") {
            relayError = value;
            return;
        }
        relaySaved = "Relay saved and sync restarted.";
        root.app.refresh();
    }

    function relayBase() {
        return root.relayText.trim().replace(/\/+$/, "");
    }

    function requestAccess() {
        accessError = "";
        accessSaved = "";
        var base = root.relayBase();
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
                accessError = "Request failed (" + xhr.status + "). Send your device ID to the operator.";
            }
        };
        xhr.send(JSON.stringify({ endpointId: id }));
    }

    function checkAccess() {
        accessError = "";
        accessSaved = "";
        var base = root.relayBase();
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
                accessSaved = "Waiting for the relay operator to approve this device.";
            else
                accessError = "No access request found for this device.";
        };
        xhr.send();
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
        height: Math.min(560, root.height - 32)
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
                text: "Corcho can sync this workspace directly between your devices with Iroh. "
                      + "No server or Git remote is required."
                color: root.th.text2
                wrapMode: Text.WordWrap
                font.family: root.th.fontFamily
                font.pixelSize: root.th.fontSizeSmall
                lineHeight: root.th.lineHeightNormal
            }

            Column {
                width: parent.width
                spacing: root.th.space2

                Text {
                    text: "Relay endpoint (optional)"
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
                    text: "Leave empty for the public n0 relays. Other iroh apps can use the same endpoint."
                    color: root.th.muted
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeCaption
                }
                Text {
                    width: parent.width
                    text: "Active relay: " + (root.status.relay ? root.status.relay : "n0 public relays")
                    color: root.th.muted
                    wrapMode: Text.WrapAnywhere
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
            }

            Column {
                width: parent.width
                spacing: root.th.space2
                visible: root.status.endpointId !== undefined && root.status.endpointId !== null

                Text {
                    text: "This device ID (for a relay allowlist)"
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
                        variant: "solid"
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
            }

            Row {
                width: parent.width
                spacing: root.th.space2
                AppButton {
                    th: root.th
                    text: "Create shared workspace"
                    variant: root.mode === "create" ? "solid" : "ghost"
                    onClicked: root.create()
                }
                AppButton {
                    th: root.th
                    text: "Join with a setup code"
                    variant: root.mode === "join" ? "solid" : "ghost"
                    onClicked: root.mode = "join"
                }
            }

            Column {
                width: parent.width
                spacing: root.th.space2
                visible: root.mode === "join"
                Text {
                    text: "Setup code"
                    color: root.th.text2
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
                }
                AppTextField {
                    width: parent.width
                    th: root.th
                    text: root.ticket
                    placeholder: "Paste the setup code from your other device"
                    onEdited: function (value) { root.ticket = value; }
                }
                Row {
                    spacing: root.th.space2
                    AppButton {
                        th: root.th
                        text: "Join"
                        variant: "solid"
                        onClicked: root.join()
                    }
                }
                Rectangle {
                    width: parent.width
                    height: root.th.borderWidth
                    color: root.th.border
                }
                Text {
                    width: parent.width
                    text: "Or pair with a word code"
                    color: root.th.text2
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
                }
                Row {
                    width: parent.width
                    spacing: root.th.space2
                    AppTextField {
                        width: parent.width - pairButton.width - root.th.space2
                        th: root.th
                        text: root.pairCode
                        placeholder: "six-little-words"
                        onEdited: function (value) { root.pairCode = value; }
                        onAccepted: root.joinPair()
                    }
                    AppButton {
                        id: pairButton
                        th: root.th
                        text: "Pair"
                        onClicked: root.joinPair()
                    }
                }
                Text {
                    width: parent.width
                    text: root.pairError
                    visible: root.pairError !== ""
                    color: root.th.danger
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
                }
            }

            Rectangle {
                width: parent.width
                height: root.th.borderWidth
                visible: root.hasWorkspace || root.shownTicket !== ""
                color: root.th.border
            }

            Column {
                width: parent.width
                spacing: root.th.space2
                visible: root.hasWorkspace || root.shownTicket !== ""
                Text {
                    width: parent.width
                    visible: root.shownTicket !== ""
                    text: "Setup code (includes the encryption key; treat it like a password)"
                    color: root.th.text2
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
                }
                TextEdit {
                    width: parent.width
                    height: 72
                    visible: root.shownTicket !== ""
                    text: root.shownTicket
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
                    AppButton {
                        th: root.th
                        visible: root.shownTicket !== ""
                        text: "Copy"
                        onClicked: root.app.copyText(root.shownTicket)
                    }
                    AppButton {
                        th: root.th
                        text: "Show write setup code"
                        onClicked: root.showTicket(false)
                    }
                    AppButton {
                        th: root.th
                        text: "Show read setup code"
                        onClicked: root.showTicket(true)
                    }
                    AppButton {
                        th: root.th
                        text: "Copy cache ticket"
                        onClicked: root.copyCacheTicket()
                    }
                }
                Text {
                    width: parent.width
                    text: "Cache tickets are read-only and omit the encryption key, for a relay-side cache. "
                          + "Enforce the relay allowlist so only cache peers receive one."
                    color: root.th.muted
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeCaption
                }
                Rectangle {
                    width: parent.width
                    height: root.th.borderWidth
                    color: root.th.border
                }
                Row {
                    spacing: root.th.space2
                    AppButton {
                        th: root.th
                        text: "Pair with words"
                        variant: "solid"
                        onClicked: root.createPair()
                    }
                    AppButton {
                        th: root.th
                        text: "Copy"
                        visible: root.shownPairCode !== ""
                        onClicked: root.app.copyText(root.shownPairCode)
                    }
                }
                Text {
                    width: parent.width
                    visible: root.shownPairCode !== ""
                    text: "Word code (valid for 10 minutes; type it on the other device)"
                    color: root.th.text2
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
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
                Text {
                    width: parent.width
                    text: root.pairError
                    visible: root.pairError !== ""
                    color: root.th.danger
                    wrapMode: Text.WordWrap
                    font.family: root.th.fontFamily
                    font.pixelSize: root.th.fontSizeSmall
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

            Item { width: 1; height: 1 }

            Text {
                width: parent.width
                text: root.status.lastError
                    ? ("Error: " + root.status.lastError)
                    : (root.status.peers > 0
                        ? (root.status.peers + " peer(s) connected")
                        : (root.status.namespace ? "Waiting for peers" : "Not configured"))
                color: root.status.lastError ? root.th.danger : root.th.muted
                wrapMode: Text.WordWrap
                font.family: root.th.fontFamily
                font.pixelSize: root.th.fontSizeCaption
            }

            Text {
                width: parent.width
                text: root.status.encrypted
                    ? "End-to-end encryption: on. Relays and caches only see ciphertext."
                    : "End-to-end encryption: off (legacy setup code)."
                color: root.status.encrypted ? root.th.success : root.th.muted
                wrapMode: Text.WordWrap
                font.family: root.th.fontFamily
                font.pixelSize: root.th.fontSizeCaption
            }

            Text {
                width: parent.width
                visible: root.status.fingerprint !== undefined && root.status.fingerprint !== null
                text: "Sync fingerprint: " + (root.status.fingerprint || "").slice(0, 16)
                color: root.th.muted
                wrapMode: Text.WrapAnywhere
                font.family: root.th.fontMono
                font.pixelSize: root.th.fontSizeCaption
            }

            Row {
                anchors.right: parent.right
                spacing: root.th.space2
                AppButton {
                    th: root.th
                    text: "Sync now"
                    enabledButton: root.status.namespace !== null && root.status.namespace !== undefined
                    onClicked: root.app.irohSyncNow()
                }
                AppButton {
                    th: root.th
                    text: "Leave"
                    variant: "danger"
                    enabledButton: root.status.namespace !== null && root.status.namespace !== undefined
                    onClicked: {
                        root.app.irohLeave();
                        root.setSetting("iroh_sync_enabled", false);
                        root.shownTicket = "";
                    }
                }
                AppButton {
                    th: root.th
                    text: "Close"
                    variant: "solid"
                    onClicked: root.close()
                }
            }
            }
        }
    }
}
