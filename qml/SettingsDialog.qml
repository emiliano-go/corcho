import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs

Item {
    id: root

    property var app
    property var th
    property bool opened: false
    property string tab: "general"
    property string keyField: ""
    property string keyStatus: ""
    property string folderMessage: ""
    property string presetName: ""
    property string presetProvider: "claude"
    property string presetModel: ""

    signal closed()
    signal gitWizardRequested()
    property bool tabHovered: false

    anchors.fill: parent
    visible: opened
    z: 350

    readonly property var s: {
        try { return JSON.parse(app.settingsJson || "{}"); } catch (e) { return {}; }
    }
    readonly property var themeList: {
        try { return JSON.parse(app.themesJson || "[]"); } catch (e) { return []; }
    }
    readonly property var fontsList: {
        try { return JSON.parse(app.fontsJson || "[]"); } catch (e) { return []; }
    }
    readonly property var modelsList: {
        try { return JSON.parse(app.modelsJson || "{}"); } catch (e) { return {}; }
    }
    readonly property var upd: {
        try { return JSON.parse(app.updateJson || "{}"); } catch (e) { return {}; }
    }
    readonly property var git: {
        try { return JSON.parse(app.gitJson || "{}"); } catch (e) { return {}; }
    }

    function set(key, value) {
        app.setSetting(key, JSON.stringify(value));
    }
    function val(key, fallback) {
        return s[key] === undefined ? fallback : s[key];
    }
    function provider() {
        return val("aiProvider", "claude");
    }
    function presets() {
        var list = val("aiPresets", []);
        return Array.isArray(list) ? list : [];
    }
    function presetModelOptions(provider) {
        return (modelsList[provider] || []).map(function (m) { return { value: m.id, label: m.name }; });
    }
    function addPreset() {
        var name = root.presetName.trim();
        if (name === "" || root.presetModel === "") return;
        var list = root.presets();
        list.push({ id: Date.now().toString(), name: name, provider: root.presetProvider, model: root.presetModel });
        root.set("aiPresets", list);
        root.presetName = "";
    }
    function removePreset(id) {
        root.set("aiPresets", root.presets().filter(function (p) { return p.id !== id; }));
    }
    function providerLabel(p) {
        return ({ claude: "Anthropic (Claude)", openai: "OpenAI (ChatGPT)", google: "Google (Gemini)", grok: "xAI (Grok)", mistral: "Mistral", local: "Local server" })[p] || p;
    }
    function modelOptions() {
        var list = modelsList[provider()] || [];
        var out = list.map(function (m) { return { value: m.id, label: m.name }; });
        if (provider() === "local") out = [{ value: val("aiModel", {}).local || "", label: "Select a model" }];
        return out;
    }
    function themeDefById(id) {
        if (id === "custom") {
            var c = val("customTheme", null);
            if (c) return { id: "custom", name: "Custom", bg: c.background, surface: c.surface, accent: c.accent, text: c.text };
        }
        for (var i = 0; i < themeList.length; i++)
            if (themeList[i].id === id) return themeList[i];
        return { id: "corcho", name: "Corcho", bg: "#121417", surface: "#1b1e23", accent: "#d98e5c", text: "#ece7de" };
    }
    function lightThemes() {
        return themeList.filter(function (t) { return t.light === true || t.id === "custom"; });
    }
    function darkThemes() {
        return themeList.filter(function (t) { return t.light !== true || t.id === "custom"; });
    }
    function systemIsDark() {
        return Application.styleHints.colorScheme === Qt.Dark;
    }
    function validateKey() {
        keyStatus = "Checking…";
        app.aiValidate(provider(), keyField);
    }
    function localPath(url) {
        var s = url.toString();
        if (s.indexOf("file://") === 0) s = s.substring(7);
        return decodeURIComponent(s);
    }
    function openDialog(which) {
        if (which === "export") exportDialog.open();
        else if (which === "import") importDialog.open();
        else if (which === "data") dataFolderDialog.open();
        else if (which === "legacy") legacyDialog.open();
        else if (which === "sync") syncFolderDialog.open();
    }

    Connections {
        target: app
        function onAiKeyResult(provider, success, message) {
            if (provider !== root.provider()) return;
            if (success) {
                var keys = Object.assign({}, root.val("aiApiKeys", {}));
                keys[provider] = root.keyField;
                root.set("aiApiKeys", keys);
                root.keyStatus = "Connected";
            } else {
                root.keyStatus = message || "Invalid API key";
            }
        }
    }

    Rectangle {
        anchors.fill: parent
        color: Qt.rgba(0, 0, 0, 0.55)
        MouseArea {
            id: dismissArea
            anchors.fill: parent
            onPressed: function(mouse) {
                var p = panel.mapFromItem(dismissArea, mouse.x, mouse.y);
                if (!panel.contains(p))
                    root.closed();
            }
        }
    }

    Rectangle {
        id: panel
        width: Math.min(920, root.width - 64)
        height: Math.min(760, root.height - 64)
        anchors.centerIn: parent
        radius: root.th.radiusPanel
        color: root.th.menu
        border.width: root.th.borderWidth
        border.color: root.th.border

        Row {
            anchors.fill: parent
            anchors.margins: 16
            spacing: root.th.space4
            Rectangle {
                width: 200
                height: parent.height
                radius: root.th.radiusCard
                color: root.th.sidebar

                Column {
                    anchors.fill: parent
                    anchors.margins: 16
                    spacing: root.th.space1
                    Repeater {
                        model: [
                            { id: "general", label: "General", icon: "settings" },
                            { id: "interface", label: "Interface", icon: "panels-top-left" },
                            { id: "appearance", label: "Appearance", icon: "palette" },
                            { id: "data", label: "Data", icon: "hard-drive" },
                            { id: "ai", label: "AI", icon: "cpu" },
                            { id: "security", label: "Security", icon: "shield" },
                            { id: "about", label: "About", icon: "info" }
                        ]

                        delegate: Item {
                            id: tabItem
                            required property var modelData
                            property bool hoveredNow: false
                            width: parent.width
                            height: 36
                            activeFocusOnTab: true
                            Accessible.role: Accessible.Button
                            Accessible.name: modelData.label + " settings"
                            Accessible.selected: root.tab === modelData.id
                            Accessible.onPressAction: root.tab = modelData.id
                            Keys.onSpacePressed: root.tab = modelData.id
                            Keys.onReturnPressed: root.tab = modelData.id
                            Keys.onEnterPressed: root.tab = modelData.id

                            Rectangle {
                                anchors.fill: parent
                                radius: root.th.radiusControl
                                color: root.tab === modelData.id ? root.th.surfaceStrong : (tabItem.hoveredNow ? root.th.hover : "transparent")
                                border.width: tabItem.activeFocus ? root.th.borderWidthFocus : 0
                                border.color: root.th.accent
                            }
                            Row {
                                anchors.left: parent.left
                                anchors.leftMargin: 12
                                anchors.verticalCenter: parent.verticalCenter
                                spacing: root.th.space2
                                Icon {
                                    anchors.verticalCenter: parent.verticalCenter
                                    name: modelData.icon
                                    size: root.th.iconBody
                                    color: root.tab === modelData.id ? root.th.accent : root.th.text2
                                }
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: modelData.label
                                    color: root.tab === modelData.id ? root.th.text : root.th.text2
                                    font.family: root.th.fontFamily
                                    font.pixelSize: root.th.fontSizeBody
                                }
                            }
                            HoverHandler { id: tabHover
                                onHoveredChanged: tabItem.hoveredNow = hovered
                            }
                            TapHandler { onTapped: root.tab = modelData.id }
                        }
                    }
                }
            }

            Column {
                width: parent.width - 216
                height: parent.height
                spacing: root.th.space3
                Item {
                    width: parent.width
                    height: 40

                    Text {
                        anchors.left: parent.left
                        anchors.right: closeButton.left
                        anchors.rightMargin: root.th.space2
                        anchors.verticalCenter: parent.verticalCenter
                        elide: Text.ElideRight
                        text: ({ general: "General", interface: "Interface", appearance: "Appearance", data: "Data", ai: "AI", security: "Security", about: "About" })[root.tab]
                        color: root.th.text
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizePage
                        font.weight: root.th.fontWeightMedium
                    }
                    Item {
                        id: closeButton
                        width: 28
                        height: 28
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        activeFocusOnTab: true
                        Accessible.role: Accessible.Button
                        Accessible.name: "Close settings"
                        Accessible.onPressAction: root.closed()
                        Keys.onSpacePressed: root.closed()
                        Keys.onReturnPressed: root.closed()
                        Keys.onEnterPressed: root.closed()

                        Rectangle {
                            anchors.fill: parent
                            radius: root.th.radiusControl
                            color: "transparent"
                            border.width: closeButton.activeFocus ? root.th.borderWidthFocus : 0
                            border.color: root.th.accent
                        }
                        Icon {
                            anchors.centerIn: parent
                            name: "x"
                            size: root.th.iconBody
                            color: root.th.muted
                        }
                        TapHandler { onTapped: root.closed() }
                    }
                }

                Flickable {
                    id: settingsScroll
                    width: parent.width
                    height: parent.height - 44
                    clip: true
                    contentHeight: content.height
                    boundsBehavior: Flickable.StopAtBounds

                    WheelHandler {
                        onWheel: function(event) {
                            var step = event.pixelDelta.y !== 0 ? event.pixelDelta.y : event.angleDelta.y;
                            if (step === 0) return;
                            settingsScroll.cancelFlick();
                            settingsScroll.contentY = Math.max(0, Math.min(settingsScroll.contentHeight - settingsScroll.height,
                                                                         settingsScroll.contentY - step));
                            event.accepted = true;
                        }
                    }

                    Column {
                        id: content
                        width: parent.width
                        spacing: root.th.space1
                        // ── General ─────────────────────────────────────────
                        Column {
                            width: parent.width
                            spacing: root.th.space1
                            visible: root.tab === "general"

                            SettingRow {
                                th: root.th
                                title: "Launch at startup"
                                description: "Open Corcho when you sign in."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("launch_at_startup", false)
                                    onToggled: function (value) { root.app.setLaunchAtStartup(value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Show changelog on update"
                                description: "Show release notes after an update."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("show_changelog", true)
                                    onToggled: function (value) { root.set("show_changelog", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Updates"
                                description: "Choose when Corcho checks for releases."
                                SelectField {
                                    width: 190
                                    th: root.th
                                    value: root.val("update_mode", "check")
                                    options: [
                                        { value: "none", label: "Do not check" },
                                        { value: "check", label: "Check automatically" },
                                        { value: "install", label: "Install automatically" }
                                    ]
                                    onSelected: function (value) { root.set("update_mode", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Update channel"
                                description: "Beta and experimental can be unstable."
                                SelectField {
                                    width: 190
                                    th: root.th
                                    value: root.val("update_channel", "stable")
                                    options: [
                                        { value: "stable", label: "Stable" },
                                        { value: "beta", label: "Beta" },
                                        { value: "experimental", label: "Experimental" }
                                    ]
                                    onSelected: function (value) { root.set("update_channel", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Update status"
                                description: root.upd.latest ? ("Version " + root.upd.latest + " is available") : (root.upd.status || "Not checked yet")
                                AppButton {
                                    th: root.th
                                    icon: "refresh-cw"
                                    text: "Check"
                                    onClicked: root.app.checkUpdates()
                                }
                                AppButton {
                                    th: root.th
                                    visible: !!root.upd.latest
                                    icon: "download"
                                    text: "Download"
                                    onClicked: root.app.downloadUpdate()
                                }
                                AppButton {
                                    th: root.th
                                    visible: !!root.upd.latest
                                    icon: "play"
                                    text: "Install"
                                    onClicked: root.app.installUpdate()
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Undo history"
                                description: "Number of steps kept for undo."
                                SliderField {
                                    width: 180
                                    th: root.th
                                    from: 5
                                    to: 100
                                    value: root.val("undo_steps", 20)
                                    onMoved: function (value) { root.set("undo_steps", Math.round(value)); }
                                }
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    width: 28
                                    text: root.val("undo_steps", 20)
                                    color: root.th.muted
                                    font.family: root.th.fontMono
                                    font.pixelSize: root.th.fontSizeSmall
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Share web sessions"
                                description: "One profile and cache for every website card. Restart to apply."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("web_shared_profile", true)
                                    onToggled: function (value) { root.set("web_shared_profile", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Sleep inactive websites"
                                description: "Unload a web card after this long to save memory. 0 keeps them awake."
                                SliderField {
                                    width: 180
                                    th: root.th
                                    from: 0
                                    to: 60
                                    value: root.val("web_sleep_minutes", 10)
                                    onMoved: function (value) { root.set("web_sleep_minutes", Math.round(value)); }
                                }
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    width: 28
                                    text: root.val("web_sleep_minutes", 10)
                                    color: root.th.muted
                                    font.family: root.th.fontMono
                                    font.pixelSize: root.th.fontSizeSmall
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Low memory mode"
                                description: "Restart required. Limits web renderers and caps script memory."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("low_memory_mode", false)
                                    onToggled: function (value) { root.set("low_memory_mode", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Launcher shortcut"
                                description: "Spawn the command palette from anywhere."
                                Keycap { th: root.th; text: "Ctrl K" }
                            }
                            SettingRow {
                                th: root.th
                                title: "Tutorials"
                                description: "Reset the in-app tutorials and tips."
                                AppButton {
                                    th: root.th
                                    icon: "rotate-ccw"
                                    text: "Reset"
                                    onClicked: root.app.resetTutorials()
                                }
                            }
                        }

                        // ── Interface ───────────────────────────────────────
                        Column {
                            width: parent.width
                            spacing: root.th.space1
                            visible: root.tab === "interface"

                            SettingRow {
                                th: root.th
                                title: "Minimap"
                                description: "Show the overview in the corner of the canvas."
                                SelectField {
                                    width: 170
                                    th: root.th
                                    value: root.val("minimap_display", "always")
                                    options: [
                                        { value: "always", label: "Always" },
                                        { value: "panning", label: "When panning" },
                                        { value: "hidden", label: "Hidden" }
                                    ]
                                    onSelected: function (value) { root.set("minimap_display", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Minimap size"
                                SelectField {
                                    width: 170
                                    th: root.th
                                    value: root.val("minimap_size", "medium")
                                    options: [
                                        { value: "small", label: "Small" },
                                        { value: "medium", label: "Medium" },
                                        { value: "large", label: "Large" }
                                    ]
                                    onSelected: function (value) { root.set("minimap_size", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Trackpad pan"
                                description: "Use two-finger scrolling to pan the grid."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("trackpad_pan", true)
                                    onToggled: function (value) { root.set("trackpad_pan", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Grid magnetism"
                                description: "Snap cards to the grid while moving them."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("grid_magnetic", true)
                                    onToggled: function (value) { root.set("grid_magnetic", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Open links"
                                description: "Where website cards open their pages."
                                SelectField {
                                    width: 170
                                    th: root.th
                                    value: root.val("link_behavior", "integrated")
                                    options: [
                                        { value: "integrated", label: "Integrated view" },
                                        { value: "browser", label: "Default browser" }
                                    ]
                                    onSelected: function (value) { root.set("link_behavior", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Card command tooltips"
                                description: "Show hints when hovering card controls."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("show_card_tooltips", true)
                                    onToggled: function (value) { root.set("show_card_tooltips", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Stylus pressure"
                                description: "Vary whiteboard stroke width with pen pressure."
                                ToggleSwitch {
                                    objectName: "whiteboardPressure"
                                    th: root.th
                                    checked: root.val("whiteboard_pressure", true)
                                    onToggled: function (value) { root.set("whiteboard_pressure", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Long-press menu"
                                description: "Hold on empty canvas to open the creation menu."
                                ToggleSwitch {
                                    objectName: "canvasLongPressMenu"
                                    th: root.th
                                    checked: root.val("canvas_long_press_menu", true)
                                    onToggled: function (value) { root.set("canvas_long_press_menu", value); }
                                }
                            }
                        }

                        // ── Appearance ──────────────────────────────────────
                        Column {
                            width: parent.width
                            spacing: root.th.space1
                            visible: root.tab === "appearance"

                            SettingRow {
                                th: root.th
                                title: "Appearance"
                                description: root.val("theme_mode", "system") === "system"
                                    ? ("Following system: " + (root.systemIsDark() ? "dark" : "light"))
                                    : "Choose a light or dark theme manually."
                                SelectField {
                                    width: 170
                                    th: root.th
                                    value: root.val("theme_mode", "system")
                                    options: [
                                        { value: "system", label: "System" },
                                        { value: "light", label: "Light" },
                                        { value: "dark", label: "Dark" }
                                    ]
                                    onSelected: function (value) { root.set("theme_mode", value); }
                                }
                            }

                            SettingRow {
                                th: root.th
                                title: "Light theme"
                                description: "Used when the system is light or the mode is Light."
                                SelectField {
                                    width: 150
                                    th: root.th
                                    value: root.val("theme_light", "paper")
                                    options: root.lightThemes().map(function (t) { return { value: t.id, label: t.name }; })
                                    onSelected: function (value) { root.set("theme_light", value); }
                                }
                                Rectangle {
                                    width: 48
                                    height: 32
                                    radius: root.th.radiusControl
                                    color: root.themeDefById(root.val("theme_light", "paper")).bg
                                    border.width: root.th.borderWidth
                                    border.color: root.th.border
                                    Rectangle {
                                        x: 6; y: 6; width: 14; height: 4; radius: 2
                                        color: root.themeDefById(root.val("theme_light", "paper")).accent
                                    }
                                    Rectangle {
                                        x: 6; y: 15; width: 28; height: 3; radius: 1.5
                                        color: root.themeDefById(root.val("theme_light", "paper")).text
                                        opacity: root.th.opacityEmphasis
                                    }
                                }
                            }

                            SettingRow {
                                th: root.th
                                title: "Dark theme"
                                description: "Used when the system is dark or the mode is Dark."
                                SelectField {
                                    width: 150
                                    th: root.th
                                    value: root.val("theme_dark", "corcho")
                                    options: root.darkThemes().map(function (t) { return { value: t.id, label: t.name }; })
                                    onSelected: function (value) { root.set("theme_dark", value); }
                                }
                                Rectangle {
                                    width: 48
                                    height: 32
                                    radius: root.th.radiusControl
                                    color: root.themeDefById(root.val("theme_dark", "corcho")).bg
                                    border.width: root.th.borderWidth
                                    border.color: root.th.border
                                    Rectangle {
                                        x: 6; y: 6; width: 14; height: 4; radius: 2
                                        color: root.themeDefById(root.val("theme_dark", "corcho")).accent
                                    }
                                    Rectangle {
                                        x: 6; y: 15; width: 28; height: 3; radius: 1.5
                                        color: root.themeDefById(root.val("theme_dark", "corcho")).text
                                        opacity: root.th.opacityEmphasis
                                    }
                                }
                            }

                            SettingRow {
                                th: root.th
                                title: "CSS themes"
                                description: "Drop .css themes in the themes folder, or start from the full template."
                                AppButton {
                                    th: root.th
                                    icon: "folder-plus"
                                    text: "Folder"
                                    onClicked: root.app.openThemesFolder()
                                }
                                AppButton {
                                    th: root.th
                                    icon: "file-text"
                                    text: "Template"
                                    onClicked: root.app.createThemeFromTemplate()
                                }
                            }

                            Column {
                                width: parent.width
                                spacing: root.th.space2
                                visible: root.val("theme_light", "paper") === "custom"
                                    || root.val("theme_dark", "corcho") === "custom"

                                Text {
                                    text: "Custom theme"
                                    color: root.th.text2
                                    font.family: root.th.fontFamily
                                    font.pixelSize: root.th.fontSizeSmall
                                }
                                Flow {
                                    width: parent.width
                                    spacing: root.th.space2
                                    Repeater {
                                        model: [
                                            { key: "background", label: "Background" },
                                            { key: "surface", label: "Surface" },
                                            { key: "accent", label: "Accent" },
                                            { key: "text", label: "Text" }
                                        ]
                                        delegate: Row {
                                            id: colorRow
                                            required property var modelData
                                            spacing: root.th.spaceCompact
                                            Text {
                                                anchors.verticalCenter: parent.verticalCenter
                                                text: colorRow.modelData.label
                                                color: root.th.muted
                                                font.family: root.th.fontFamily
                                                font.pixelSize: root.th.fontSizeSmall
                                            }
                                            AppTextField {
                                                width: 96
                                                th: root.th
                                                inputHeight: 30
                                                text: {
                                                    var c = root.val("customTheme", null) || {};
                                                    return c[colorRow.modelData.key] || "#121417";
                                                }
                                                onCommitted: function (value) {
                                                    var c = Object.assign({}, root.val("customTheme", null) || {});
                                                    c[colorRow.modelData.key] = value;
                                                    root.set("customTheme", c);
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            Item { width: 1; height: 8 }

                            SettingRow {
                                th: root.th
                                title: "Font"
                                SelectField {
                                    width: 190
                                    th: root.th
                                    value: root.val("font", "inter")
                                    options: root.fontsList.map(function (f) { return { value: f.id, label: f.name }; })
                                    onSelected: function (value) { root.set("font", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Card roundness"
                                description: "Corner radius of every card."
                                SliderField {
                                    width: 180
                                    th: root.th
                                    from: 0
                                    to: 28
                                    value: root.val("card_radius", 14)
                                    onMoved: function (value) { root.set("card_radius", Math.round(value)); }
                                }
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    width: 28
                                    text: Math.round(root.val("card_radius", 14))
                                    color: root.th.muted
                                    font.family: "JetBrains Mono"
                                    font.pixelSize: 11
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Grid opacity"
                                SliderField {
                                    width: 180
                                    th: root.th
                                    from: 10
                                    to: 100
                                    value: Math.round(root.val("grid_opacity", 0.6) * 100)
                                    onMoved: function (value) { root.set("grid_opacity", value / 100); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Master volume"
                                SliderField {
                                    width: 180
                                    th: root.th
                                    from: 0
                                    to: 100
                                    value: root.val("master_volume", 50)
                                    onMoved: function (value) { root.set("master_volume", Math.round(value)); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Spawn animations"
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("spawn_animations", true)
                                    onToggled: function (value) { root.set("spawn_animations", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Card parallax"
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("card_parallax", true)
                                    onToggled: function (value) { root.set("card_parallax", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Grid ripple on click"
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("grid_ripple", true)
                                    onToggled: function (value) { root.set("grid_ripple", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Reduced motion"
                                description: "Disable non-essential animation."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("reduced_motion", false)
                                    onToggled: function (value) { root.set("reduced_motion", value); }
                                }
                            }
                        }

                        // ── Data ────────────────────────────────────────────
                        Column {
                            width: parent.width
                            spacing: root.th.space3
                            visible: root.tab === "data"

                            SettingsSection {
                                th: root.th
                                title: "Cloud sync"
                                icon: "cloud"
                                description: "Sync your grids across devices through a folder or Google Drive."

                                SettingRow {
                                    th: root.th
                                    title: "Enable cloud sync"
                                    description: "Turn folder or Google Drive sync on or off."
                                    ToggleSwitch {
                                        th: root.th
                                        checked: root.val("cloud_sync_enabled", false)
                                        onToggled: function (value) { root.set("cloud_sync_enabled", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Provider"
                                    SelectField {
                                        width: 180
                                        th: root.th
                                        value: root.val("cloud_provider", "none")
                                        options: [
                                            { value: "none", label: "None" },
                                            { value: "folder", label: "Folder" },
                                            { value: "gdrive", label: "Google Drive" }
                                        ]
                                        onSelected: function (value) { root.set("cloud_provider", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Sync folder"
                                    description: root.val("cloud_folder", "") || "No folder selected"
                                    AppButton {
                                        th: root.th
                                        icon: "folder-plus"
                                        text: "Choose"
                                        onClicked: root.openDialog("sync")
                                    }
                                    AppButton {
                                        th: root.th
                                        icon: "refresh-cw"
                                        text: "Sync now"
                                        onClicked: root.app.syncNow()
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Google OAuth client ID"
                                    description: "From a Google Cloud OAuth desktop client."
                                    AppTextField {
                                        width: 260
                                        th: root.th
                                        text: root.val("gdrive_client_id", "")
                                        placeholder: "…apps.googleusercontent.com"
                                        onCommitted: function (value) { root.set("gdrive_client_id", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Google OAuth client secret"
                                    description: "Optional for public desktop clients."
                                    AppTextField {
                                        width: 260
                                        th: root.th
                                        password: true
                                        text: root.val("gdrive_client_secret", "")
                                        placeholder: "Client secret"
                                        onCommitted: function (value) { root.set("gdrive_client_secret", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Google Drive"
                                    description: root.val("gdrive_connected", false) ? (root.val("gdrive_email", "") || "Connected") : "Not connected"
                                    AppButton {
                                        th: root.th
                                        icon: "cloud"
                                        text: root.val("gdrive_connected", false) ? "Disconnect" : "Connect"
                                        onClicked: {
                                            if (root.val("gdrive_connected", false)) root.app.gdriveDisconnect();
                                            else root.app.gdriveConnect();
                                        }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Sync files to cloud"
                                    description: "Include attachments and images."
                                    ToggleSwitch {
                                        th: root.th
                                        checked: root.val("cloud_file_sync", false)
                                        onToggled: function (value) { root.set("cloud_file_sync", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Show sync indicator"
                                    ToggleSwitch {
                                        th: root.th
                                        checked: root.val("cloud_indicator", false)
                                        onToggled: function (value) { root.set("cloud_indicator", value); }
                                    }
                                }
                            }

                            SettingsSection {
                                th: root.th
                                title: "Git sync"
                                icon: "git-branch"
                                description: "Keep the workspace in a Git repository on GitHub, Codeberg, GitLab or any remote."

                                SettingRow {
                                    th: root.th
                                    title: "Enable git sync"
                                    description: "Pull on start and push changes after edits."
                                    AppButton {
                                        th: root.th
                                        icon: "git-compare"
                                        text: "Set up…"
                                        onClicked: root.gitWizardRequested()
                                    }
                                    ToggleSwitch {
                                        th: root.th
                                        checked: root.val("git_sync_enabled", false)
                                        onToggled: function (value) { root.set("git_sync_enabled", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Remote"
                                    description: "SSH or HTTPS URL. The system git credentials and SSH agent are used."
                                    AppTextField {
                                        width: 300
                                        th: root.th
                                        text: root.val("git_remote", "")
                                        placeholder: "git@github.com:you/corcho-data.git"
                                        onCommitted: function (value) { root.set("git_remote", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Authentication"
                                    description: "gh CLI uses the login from the gh command instead of stored tokens."
                                    SelectField {
                                        width: 170
                                        th: root.th
                                        value: root.val("git_auth", "system")
                                        options: [
                                            { value: "system", label: "Git credentials" },
                                            { value: "gh", label: "gh CLI" }
                                        ]
                                        onSelected: function (value) { root.set("git_auth", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Branch"
                                    AppTextField {
                                        width: 160
                                        th: root.th
                                        text: root.val("git_branch", "main")
                                        placeholder: "main"
                                        onCommitted: function (value) { root.set("git_branch", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Pull at start"
                                    description: "Fast-forward or union-merge the remote before loading."
                                    ToggleSwitch {
                                        th: root.th
                                        checked: root.val("git_auto_pull", true)
                                        onToggled: function (value) { root.set("git_auto_pull", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Push changes"
                                    description: "Commit and push two seconds after the last edit."
                                    ToggleSwitch {
                                        th: root.th
                                        checked: root.val("git_auto_push", true)
                                        onToggled: function (value) { root.set("git_auto_push", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Include attachments"
                                    ToggleSwitch {
                                        th: root.th
                                        checked: root.val("git_include_attachments", false)
                                        onToggled: function (value) { root.set("git_include_attachments", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "GitHub card token"
                                    description: "Used by Git cards for private repositories when Authentication is Git credentials."
                                    AppTextField {
                                        width: 220
                                        th: root.th
                                        password: true
                                        text: root.val("repo_token_github", "")
                                        placeholder: "Token"
                                        onCommitted: function (value) { root.set("repo_token_github", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "GitLab card token"
                                    description: "Used by Git cards for private repositories."
                                    AppTextField {
                                        width: 220
                                        th: root.th
                                        password: true
                                        text: root.val("repo_token_gitlab", "")
                                        placeholder: "Token"
                                        onCommitted: function (value) { root.set("repo_token_gitlab", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Forgejo card token"
                                    description: "Used by Git cards for Forgejo and Gitea repositories."
                                    AppTextField {
                                        width: 220
                                        th: root.th
                                        password: true
                                        text: root.val("repo_token_forgejo", "")
                                        placeholder: "Token"
                                        onCommitted: function (value) { root.set("repo_token_forgejo", value); }
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Git status"
                                    description: root.git.lastError ? ("Error: " + root.git.lastError) : (root.git.lastSync ? ("Last sync: " + root.git.lastSync) : "Not synced yet")
                                    AppButton {
                                        th: root.th
                                        icon: "git-compare"
                                        text: "Sync now"
                                        enabledButton: root.val("git_sync_enabled", false) && root.val("git_remote", "") !== ""
                                        onClicked: root.app.gitSyncNow()
                                    }
                                }
                            }

                            SettingsSection {
                                th: root.th
                                title: "Backup and storage"
                                icon: "hard-drive"
                                description: "Export, import or move the workspace data."

                                SettingRow {
                                    th: root.th
                                    title: "Backup"
                                    description: "Export or import all grids and cards."
                                    AppButton {
                                        th: root.th
                                        icon: "upload"
                                        text: "Export"
                                        onClicked: root.openDialog("export")
                                    }
                                    AppButton {
                                        th: root.th
                                        icon: "download"
                                        text: "Import"
                                        onClicked: root.openDialog("import")
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Data folder"
                                    description: root.val("data_dir", "") || "Default location"
                                    AppButton {
                                        th: root.th
                                        icon: "folder-plus"
                                        text: "Choose"
                                        onClicked: root.openDialog("data")
                                    }
                                }
                                SettingRow {
                                    th: root.th
                                    title: "Import legacy data"
                                    description: "Read a legacy data folder."
                                    AppButton {
                                        th: root.th
                                        icon: "download"
                                        text: "Import"
                                        onClicked: root.openDialog("legacy")
                                    }
                                }
                                Text {
                                    width: parent.width
                                    text: root.folderMessage
                                    visible: root.folderMessage !== ""
                                    color: root.th.muted
                                    font.family: root.th.fontFamily
                                    font.pixelSize: root.th.fontSizeSmall
                                    wrapMode: Text.WordWrap
                                }
                            }
                        }

                        // ── AI ──────────────────────────────────────────────
                        Column {
                            width: parent.width
                            spacing: root.th.space1
                            visible: root.tab === "ai"

                            SettingRow {
                                th: root.th
                                title: "Provider"
                                SelectField {
                                    width: 190
                                    th: root.th
                                    value: root.provider()
                                    options: [
                                        { value: "claude", label: "Anthropic (Claude)" },
                                        { value: "openai", label: "OpenAI (ChatGPT)" },
                                        { value: "google", label: "Google (Gemini)" },
                                        { value: "grok", label: "xAI (Grok)" },
                                        { value: "mistral", label: "Mistral" },
                                        { value: "openrouter", label: "OpenRouter" },
                                        { value: "opencode-go", label: "OpenCode Go" },
                                        { value: "local", label: "Local server" }
                                    ]
                                    onSelected: function (value) { root.set("aiProvider", value); root.keyStatus = ""; }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "API key"
                                description: root.keyStatus !== "" ? root.keyStatus : "Stored locally, never synced."
                                AppTextField {
                                    width: 210
                                    th: root.th
                                    password: true
                                    placeholder: "Paste key"
                                    onEdited: function (value) { root.keyField = value; }
                                    onAccepted: root.validateKey()
                                }
                                AppButton {
                                    th: root.th
                                    icon: "plug"
                                    text: "Connect"
                                    onClicked: root.validateKey()
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Model"
                                SelectField {
                                    width: 210
                                    th: root.th
                                    value: {
                                        var models = root.val("aiModel", {});
                                        return models[root.provider()] || "";
                                    }
                                    placeholder: "Select a model"
                                    options: root.modelOptions()
                                    onSelected: function (value) {
                                        var models = Object.assign({}, root.val("aiModel", {}));
                                        models[root.provider()] = value;
                                        root.set("aiModel", models);
                                    }
                                }
                            }
                            Column {
                                width: parent.width
                                spacing: root.th.space1
                                Text {
                                    text: "AI presets"
                                    color: root.th.text2
                                    font.family: root.th.fontFamily
                                    font.pixelSize: root.th.fontSizeSmall
                                    font.weight: root.th.fontWeightMedium
                                }
                                Repeater {
                                    model: root.presets()
                                    delegate: Item {
                                        required property var modelData
                                        width: parent.width
                                        height: 34
                                        Row {
                                            anchors.fill: parent
                                            spacing: root.th.space2
                                            Text {
                                                width: parent.width - 250
                                                anchors.verticalCenter: parent.verticalCenter
                                                text: modelData.name
                                                color: root.th.text
                                                elide: Text.ElideRight
                                                font.family: root.th.fontFamily
                                                font.pixelSize: root.th.fontSizeBody
                                            }
                                            Text {
                                                width: 130
                                                anchors.verticalCenter: parent.verticalCenter
                                                text: root.providerLabel(modelData.provider)
                                                color: root.th.muted
                                                elide: Text.ElideRight
                                                font.family: root.th.fontFamily
                                                font.pixelSize: root.th.fontSizeSmall
                                            }
                                            Text {
                                                width: 90
                                                anchors.verticalCenter: parent.verticalCenter
                                                text: modelData.model
                                                color: root.th.muted
                                                elide: Text.ElideRight
                                                font.family: root.th.fontMono
                                                font.pixelSize: root.th.fontSizeSmall
                                            }
                                            AppButton {
                                                th: root.th
                                                icon: "trash-2"
                                                onClicked: root.removePreset(modelData.id)
                                            }
                                        }
                                    }
                                }
                                Row {
                                    width: parent.width
                                    spacing: root.th.space2
                                    AppTextField {
                                        width: 130
                                        th: root.th
                                        placeholder: "Preset name"
                                        onEdited: function (value) { root.presetName = value; }
                                    }
                                    SelectField {
                                        width: 150
                                        th: root.th
                                        value: root.presetProvider
                                        options: [
                                            { value: "claude", label: "Claude" },
                                            { value: "openai", label: "OpenAI" },
                                            { value: "google", label: "Google" },
                                            { value: "grok", label: "Grok" },
                                            { value: "mistral", label: "Mistral" },
                                            { value: "openrouter", label: "OpenRouter" },
                                            { value: "opencode-go", label: "OpenCode Go" },
                                            { value: "local", label: "Local" }
                                        ]
                                        onSelected: function (value) {
                                            root.presetProvider = value;
                                            var options = root.presetModelOptions(value);
                                            root.presetModel = options.length > 0 ? options[0].value : "";
                                        }
                                    }
                                    SelectField {
                                        width: 160
                                        th: root.th
                                        value: root.presetModel
                                        placeholder: "Model"
                                        options: root.presetModelOptions(root.presetProvider)
                                        onSelected: function (value) { root.presetModel = value; }
                                    }
                                    AppButton {
                                        th: root.th
                                        icon: "plus"
                                        text: "Add"
                                        enabledButton: root.presetName.trim() !== "" && root.presetModel !== ""
                                        onClicked: root.addPreset()
                                    }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Local server"
                                description: "An OpenAI-compatible endpoint."
                                AppTextField {
                                    width: 220
                                    th: root.th
                                    text: root.val("ai_local_server_url", "")
                                    placeholder: "http://127.0.0.1:8080/v1"
                                    onCommitted: function (value) { root.set("ai_local_server_url", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Enable web search"
                                description: "Ask the model to use current web knowledge."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("ai_web_search", true)
                                    onToggled: function (value) { root.set("ai_web_search", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Stream responses"
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("ai_stream", true)
                                    onToggled: function (value) { root.set("ai_stream", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Refresh interval"
                                description: "How often AI cards re-run."
                                SelectField {
                                    width: 180
                                    th: root.th
                                    value: root.val("ai_refresh_interval", "launch")
                                    options: [
                                        { value: "manual", label: "Manually" },
                                        { value: "launch", label: "At launch" },
                                        { value: "hour", label: "Every hour" },
                                        { value: "5min", label: "Every 5 minutes" },
                                        { value: "minute", label: "Every minute" }
                                    ]
                                    onSelected: function (value) { root.set("ai_refresh_interval", value); }
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Total AI cards"
                                description: "A high count with frequent refreshes can cost money."
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: {
                                        try {
                                            var grids = JSON.parse(root.app.gridsJson);
                                            var n = 0;
                                            grids.forEach(function (g) {
                                                g.cards.forEach(function (c) { if (c.type === "ai") n++; });
                                            });
                                            return "" + n;
                                        } catch (e) { return "0"; }
                                    }
                                    color: root.th.text
                                    font.family: root.th.fontMono
                                    font.pixelSize: root.th.fontSizeSmall
                                }
                            }
                        }

                        // ── Security ────────────────────────────────────────
                        Column {
                            width: parent.width
                            spacing: root.th.space1
                            visible: root.tab === "security"

                            SettingRow {
                                th: root.th
                                title: "Website ad blocker"
                                description: "Blocks known ad and tracking hosts in website cards."
                                ToggleSwitch {
                                    th: root.th
                                    checked: root.val("ad_blocker", false)
                                    onToggled: function (value) { root.set("ad_blocker", value); }
                                }
                            }
                            Text {
                                width: parent.width
                                topPadding: 12
                                color: root.th.text2
                                text: "How your data is stored"
                                font.family: root.th.fontFamily
                                font.pixelSize: root.th.fontSizeBody
                                font.weight: root.th.fontWeightMedium
                            }
                            Text {
                                width: parent.width
                                color: root.th.muted
                                text: "Grids, cards and attachments stay on this device unless Cloud Sync is on. API keys are stored locally and are never synced. AI prompts go directly to the provider you choose. Websites run in an isolated session per card and cannot read your other websites, local files, or Corcho data."
                                font.family: root.th.fontFamily
                                font.pixelSize: root.th.fontSizeSmall
                                wrapMode: Text.WordWrap
                                lineHeight: 1.35
                            }
                            Text {
                                width: parent.width
                                topPadding: 12
                                color: root.th.muted
                                text: "Corcho sends no telemetry. There is no analytics client and no remote logging."
                                font.family: root.th.fontFamily
                                font.pixelSize: root.th.fontSizeSmall
                                wrapMode: Text.WordWrap
                                lineHeight: 1.35
                            }
                        }

                        // ── About ───────────────────────────────────────────
                        Column {
                            width: parent.width
                            spacing: root.th.spaceCompact
                            visible: root.tab === "about"

                            SettingRow {
                                th: root.th
                                title: "Corcho"
                                description: "Notes, sites, tasks and calendars on one infinite grid."
                                Text {
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: "v" + root.app.version
                                    color: root.th.text2
                                    font.family: root.th.fontMono
                                    font.pixelSize: root.th.fontSizeSmall
                                }
                            }
                            SettingRow {
                                th: root.th
                                title: "Tutorials"
                                description: "Replay the welcome sequence."
                                AppButton {
                                    th: root.th
                                    icon: "rotate-ccw"
                                    text: "Replay"
                                    onClicked: root.app.resetTutorials()
                                }
                            }
                            Text {
                                width: parent.width
                                topPadding: 10
                                text: root.app.changelog()
                                color: root.th.muted
                                font.family: root.th.fontFamily
                                font.pixelSize: root.th.fontSizeSmall
                                wrapMode: Text.WordWrap
                                lineHeight: 1.35
                            }
                            Row {
                                spacing: root.th.space4
                                topPadding: 10
                                Text {
                                    text: "Project"
                                    color: root.th.accentText
                                    font.family: root.th.fontFamily
                                    font.pixelSize: root.th.fontSizeSmall
                                    TapHandler { onTapped: root.app.openExternal("https://github.com/emiliano-go/repo") }
                                }
                                Text {
                                    text: "Privacy"
                                    color: root.th.accentText
                                    font.family: root.th.fontFamily
                                    font.pixelSize: root.th.fontSizeSmall
                                    TapHandler { onTapped: root.app.openExternal("https://sfg545.dev/orchard") }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    FileDialog {
        id: exportDialog
        title: "Export backup"
        fileMode: FileDialog.SaveFile
        nameFilters: ["Corcho backup (*.corcho)", "All files (*)"]
        onAccepted: root.app.exportBackup(root.localPath(selectedFile))
    }

    FileDialog {
        id: importDialog
        title: "Import backup"
        fileMode: FileDialog.OpenFile
        nameFilters: ["Corcho backup (*.corcho)", "All files (*)"]
        onAccepted: root.app.importBackup(root.localPath(selectedFile))
    }

    FolderDialog {
        id: dataFolderDialog
        title: "Select data folder"
        onAccepted: {
            if (root.app.setDataDir(root.localPath(selectedFolder))) root.folderMessage = "Data folder updated";
            else root.folderMessage = "Could not use that folder";
        }
    }

    FolderDialog {
        id: legacyDialog
        title: "Select legacy data folder"
        onAccepted: {
            if (root.app.importLegacy(root.localPath(selectedFolder))) root.folderMessage = "Legacy data imported";
            else root.folderMessage = "No importable data found";
        }
    }

    FolderDialog {
        id: syncFolderDialog
        title: "Select sync folder"
        onAccepted: root.set("cloud_folder", root.localPath(selectedFolder))
    }
}
