import QtQuick
import QtQuick.Controls
import QtQuick.Window
import QtQuick.Dialogs
import Corcho 1.0

ApplicationWindow {
    id: root

    visible: true
    width: 1440
    height: 900
    minimumWidth: 900
    minimumHeight: 600
    title: "Corcho"
    color: root.th.bg

    FontLoader { id: fontRegular; source: "qrc:/assets/fonts/inter_regular.ttf" }
    FontLoader { id: fontMedium; source: "qrc:/assets/fonts/inter_medium.ttf" }
    FontLoader { id: fontSemi; source: "qrc:/assets/fonts/inter_semibold.ttf" }
    FontLoader { id: fontBrand; source: "qrc:/assets/fonts/texgyreadventor-bold.otf" }

    AppController { id: app }

    function parseJson(text, fallback) {
        try { return JSON.parse(text || ""); } catch (e) { return fallback; }
    }

    function filePath(url) {
        var s = url.toString();
        if (s.indexOf("file://") === 0) s = s.substring(7);
        return decodeURIComponent(s);
    }

    readonly property var settings: parseJson(app.settingsJson, {})
    readonly property var grids: parseJson(app.gridsJson, [])
    readonly property var cards: parseJson(app.cardsJson, [])
    readonly property var allCards: parseJson(app.allCardsJson, [])
    readonly property var folders: parseJson(app.foldersJson, [])
    readonly property var themes: parseJson(app.themesJson, [])
    readonly property var fonts: parseJson(app.fontsJson, [])

    readonly property var activeGrid: {
        for (var i = 0; i < grids.length; i++)
            if (grids[i].id === app.activeGridId) return grids[i];
        return null;
    }

    readonly property bool systemDark: Application.styleHints.colorScheme === Qt.Dark
    readonly property var th: parseJson(appearance.tokensJson, {})

    AppearanceController { id: appearance }

    function refreshAppearance() {
        appearance.refresh(app.settingsJson, app.themesJson, app.fontsJson, systemDark);
    }

    onSystemDarkChanged: refreshAppearance()

    property bool launcherOpen: false
    property bool settingsOpen: false
    property string settingsTab: "general"
    property bool onboardingOpen: false
    property bool onboardingDismissed: false
    property bool sidebarCollapsed: false

    // True while a dialog, wizard or popup covers the canvas. The canvas
    // ignores wheel and navigation input so it cannot scroll behind a modal.
    readonly property bool modalOpen: launcher.opened
        || settingsOpen
        || onboarding.opened
        || irohWizard.opened
        || templatesDialog.opened
        || dateLinkDialog.opened
        || calendarManager.opened
        || prompt.opened
        || contextMenu.visible

    property var menuActions: []
    property var menuCard: null
    property var menuCardIds: []
    property bool menuLockTarget: false
    property string promptKind: ""
    property var templateTarget: []
    property var menuGrid: null
    property var menuFolder: null
    property var menuGroup: null
    property var menuWorld: ({ x: 0, y: 0 })
    property var renameTarget: ({ kind: "", id: "" })
    property var pendingGroupIds: []
    property string pendingGroupId: ""
    // Card id -> FloatingCardWindow. Reassigned whenever it changes so QML
    // bindings that depend on the id list are notified.
    property var floatingCards: ({})
    // Creation order, used to lay the detached windows out left to right.
    property var floatingOrder: []
    // Stable token used in each detached window's title so compositor IPC can
    // find the right toplevel.
    property int floatingTokenSeed: 0
    property int floatingReflowPasses: 0
    readonly property var floatingIds: Object.keys(floatingCards)

    // Keep the detached windows in one row with a constant gap. This runs
    // after a batch has been created, and once more after the windows are
    // mapped so a window manager cannot leave a fresh window behind another.
    readonly property int floatingGap: 14

    function reflowFloatingWindows() {
        var margin = 24;
        var screenW = root.screen ? root.screen.width : Screen.width;
        var screenH = root.screen ? root.screen.height : Screen.height;
        var baseX = Math.max(margin, Math.min(root.x + 48, screenW - margin - 280));
        var baseY = Math.max(margin, Math.min(root.y + 96, screenH - margin - 200));
        var x = baseX;
        var y = baseY;
        var rowHeight = 0;
        var placements = [];

        for (var i = 0; i < root.floatingOrder.length; i++) {
            var win = root.floatingCards[root.floatingOrder[i]];
            if (!win) continue;
            var w = win.width > 0 ? win.width : 360;
            var h = win.height > 0 ? win.height : 260;
            if (x > baseX && x + w > screenW - margin) {
                x = baseX;
                y += rowHeight + root.floatingGap;
                rowHeight = 0;
            }
            win.x = x;
            win.y = y;
            if (win.floatingToken)
                placements.push({ token: win.floatingToken, x: x, y: y });
            x += w + root.floatingGap;
            rowHeight = Math.max(rowHeight, h);
        }

        // On sway the compositor owns toplevel positions, so ask it to move
        // the floating PiP windows to the layout computed above.
        if (placements.length > 0 && app)
            app.placeFloatingWindows(JSON.stringify(placements));
    }

    function forgetFloatingCard(id) {
        if (floatingCards[id]) {
            var next = Object.assign({}, floatingCards);
            delete next[id];
            floatingCards = next;
        }
        var order = floatingOrder.filter(function (value) { return value !== id; });
        if (order.length !== floatingOrder.length)
            floatingOrder = order;
        // Do not reflow the remaining windows here: returning one card must
        // leave the other PiP windows exactly where the user has them.
        root.floatingReflowPasses = 0;
        floatingReflowTimer.stop();
    }

    function closeFloatingCard(id) {
        var win = floatingCards[id];
        if (win) win.close();
        else forgetFloatingCard(id);
    }

    function createFloatingCardWindow(id, card) {
        var component = Qt.createComponent("FloatingCardWindow.qml");
        if (component.status !== Component.Ready) {
            toastHost.show("error", "Could not open picture-in-picture window");
            return null;
        }

        root.floatingTokenSeed += 1;
        var token = String(root.floatingTokenSeed);
        // Website cards hand their loaded page to the window instead of
        // loading the site a second time.
        var webView = card.type === "website" ? canvas.releaseCardWebView(id) : null;
        // Live body state that is not persisted in the card payload, such as
        // a running Pomodoro timer, moves with the card into the PiP window.
        var sourceBody = canvas.bodyItem(id);
        var runtime = sourceBody && typeof sourceBody.exportRuntime === "function"
            ? sourceBody.exportRuntime() : null;
        var win = component.createObject(null, {
            "app": app,
            "th": root.th,
            "cardId": id,
            "cardData": card,
            "bodySource": canvas.bodyFor(card.type),
            "gridId": app.activeGridId,
            "webProfile": canvas.webProfile,
            "externalWebView": webView,
            "floatingToken": token,
            "initialWidth": Math.max(320, card.width),
            "initialHeight": Math.max(260, card.height) + 38,
            "x": root.x + 48,
            "y": root.y + 96
        });
        if (!win) {
            // Put the page back so the canvas copy keeps working.
            if (webView) canvas.acceptCardWebView(id, webView);
            toastHost.show("error", "Could not open picture-in-picture window");
            return null;
        }

        win.th = Qt.binding(function () { return root.th; });
        win.webProfile = Qt.binding(function () { return canvas.webProfile; });
        win.cardReturned.connect(function () { root.forgetFloatingCard(id); });
        win.runtimeReturned.connect(function (state) {
            var body = canvas.bodyItem(id);
            if (body && typeof body.importRuntime === "function")
                body.importRuntime(state);
        });
        win.webViewReturned.connect(function (view) {
            // The window is closing: give the page back to the canvas copy,
            // or drop it when that copy no longer exists.
            if (!canvas.acceptCardWebView(id, view)) view.destroy();
        });
        win.dateLinkRequested.connect(function (sourceCardId, sourceItemId, linkId, defaultDate) {
            dateLinkDialog.openFor(sourceCardId, sourceItemId, linkId, defaultDate);
        });
        win.calendarManagerRequested.connect(function () { calendarManager.open(); });
        if (runtime) win.applyRuntime(runtime);

        var next = Object.assign({}, floatingCards);
        next[id] = win;
        floatingCards = next;
        var order = floatingOrder.slice();
        order.push(id);
        floatingOrder = order;
        // Position immediately so the new window does not flash at the old
        // cascade point before the end of the batch.
        root.reflowFloatingWindows();
        return win;
    }

    function openFloatingCards(ids) {
        var opened = false;
        for (var i = 0; i < ids.length; i++) {
            var id = ids[i];
            if (floatingCards[id]) {
                floatingCards[id].raise();
                continue;
            }
            var card = canvas.cardById(id);
            if (!card) continue;
            if (root.createFloatingCardWindow(id, card)) opened = true;
        }

        if (opened) {
            root.reflowFloatingWindows();
            // Reapply once the windows are mapped: some window managers move
            // a fresh floating window after the initial position request.
            Qt.callLater(root.reflowFloatingWindows);
            root.floatingReflowPasses = 3;
            floatingReflowTimer.restart();
        }
        canvas.setSelection([]);
    }

    function openFloatingCard(id) {
        root.openFloatingCards([id]);
    }

    function syncFloatingCards() {
        if (root.floatingIds.length === 0) return;
        var live = {};
        for (var i = 0; i < root.allCards.length; i++) live[root.allCards[i].id] = root.allCards[i];
        for (var id in floatingCards) {
            var win = floatingCards[id];
            if (!win) continue;
            var card = live[id];
            if (!card) {
                // Deleted elsewhere: close the detached copy as well. The
                // window's onClosing handler removes it from the map.
                win.close();
                continue;
            }
            win.cardData = card;
        }
    }

    onAllCardsChanged: syncFloatingCards()

    // Reflow again after the windows have been mapped.
    Timer {
        id: floatingReflowTimer
        interval: 90
        repeat: true
        running: false
        onTriggered: {
            root.reflowFloatingWindows();
            root.floatingReflowPasses -= 1;
            if (root.floatingReflowPasses <= 0) stop();
        }
    }

    function openSettings(tab) {
        settingsTab = tab || "general";
        settingsOpen = true;
    }

    function showMenu(px, py, actions) {
        menuActions = actions;
        contextMenu.openAt(px, py);
    }

    function createCardActions() {
        return [
            { label: "Text", icon: "file-text", children: [
                { label: "Note", icon: "file-text", action: "create:note" },
                { label: "Checklist", icon: "list-checks", action: "create:checklist" },
                { label: "Kanban", icon: "kanban", action: "create:kanban" },
                { label: "Whiteboard", icon: "pencil", action: "create:whiteboard" },
                { label: "Legend", icon: "list", action: "create:legend" }
            ] },
            { label: "Web", icon: "globe", children: [
                { label: "Website", icon: "globe", action: "create:website" },
                { label: "Bookmark", icon: "bookmark", action: "create:bookmark" },
                { label: "API data", icon: "database", action: "create:api" },
                { label: "Grid portal", icon: "arrow-right", action: "create:portal" }
            ] },
            { label: "Media & files", icon: "folder", children: [
                { label: "File", icon: "file-text", action: "pick-file" },
                { label: "Folder", icon: "folder-plus", action: "pick-folder" },
                { label: "Timbre", icon: "audio-lines", action: "create:timbre" },
                { label: "Now playing", icon: "music-2", action: "create:mpris" }
            ] },
            { label: "Tools", icon: "cpu", children: [
                { label: "AI node", icon: "cpu", action: "create:ai" },
                { label: "Script", icon: "terminal", action: "create:script" },
                { label: "Calendar", icon: "calendar", action: "create:calendar" },
                { label: "Pomodoro", icon: "timer", action: "create:pomodoro" },
                { label: "Git repository", icon: "git-branch", action: "create:git" }
            ] }
        ];
    }

    // Create a card from a menu or double-click and focus its editor when the
    // card type supports typing. app.createCard returns the new card as JSON.
    function createCard(kind, wx, wy) {
        var card = parseJson(String(app.createCard(kind, wx, wy)), null);
        if (card && (card.type === "note" || card.type === "checklist"))
            canvas.focusCard(card.id);
        return card;
    }

    function openCreateMenu(px, py, wx, wy) {
        menuWorld = { x: wx, y: wy };
        showMenu(px, py, [
            { label: "Add card", icon: "plus", children: createCardActions() }
        ]);
    }

    function openCanvasMenu(px, py, wx, wy) {
        menuWorld = { x: wx, y: wy };
        showMenu(px, py, createCardActions());
    }

    function openCardMenu(card, px, py) {
        menuCard = card;
        var isFloating = root.floatingIds.indexOf(card.id) >= 0;
        var multi = canvas.selectedIds.length > 1 && !isFloating;
        var ids = multi ? canvas.selectedIds.slice() : [card.id];
        if (ids.indexOf(card.id) < 0) ids.push(card.id);
        menuCardIds = ids;

        var selectedCards = [];
        for (var c = 0; c < ids.length; c++) {
            var selected = canvas.cardById(ids[c]);
            if (selected) selectedCards.push(selected);
        }
        var allLocked = selectedCards.length > 0;
        var allGrouped = selectedCards.length > 0;
        var allSynced = selectedCards.length > 0;
        var allFloating = selectedCards.length > 0;
        for (var s = 0; s < selectedCards.length; s++) {
            if (!selectedCards[s].locked) allLocked = false;
            if (!selectedCards[s].group) allGrouped = false;
            if (!selectedCards[s].syncId) allSynced = false;
            if (root.floatingIds.indexOf(selectedCards[s].id) < 0) allFloating = false;
        }
        menuLockTarget = !allLocked;
        var groupId = allGrouped ? selectedCards[0].group : "";
        root.menuGroup = allGrouped
            ? { id: groupId, name: selectedCards[0].group_name || "Group" }
            : null;
        var incoming = 0;
        if (!multi) {
            for (var inc = 0; inc < allCards.length; inc++) {
                var incLinks = allCards[inc].links || [];
                for (var il = 0; il < incLinks.length; il++)
                    if (incLinks[il].to === card.id) incoming++;
            }
        }
        var outgoing = (card.links || []).length;

        var actions = [];
        if (allFloating) {
            actions.push({
                label: multi ? "Return cards to canvas" : "Return card to canvas",
                icon: "undo-2",
                action: "card:unpip"
            });
        } else {
            actions.push({
                label: multi ? "Open cards in picture-in-picture" : "Open in picture-in-picture",
                icon: "app-window",
                action: "card:pip"
            });
            actions.push({ separator: true, label: "" });
        }
        actions.push({ label: multi ? "Duplicate cards" : "Duplicate card", icon: "copy", action: "card:duplicate" });
        actions.push({ label: multi ? "Pin colours" : "Pin colour", icon: "palette", action: "card:color" });
        if (!multi)
            actions.push({ label: "Link date", icon: "calendar", action: "card:link-date" });
        actions.push({
            label: multi
                ? (menuLockTarget ? "Pin cards" : "Unpin cards")
                : (menuLockTarget ? "Pin card" : "Unpin card"),
            icon: menuLockTarget ? "pin" : "pin-off",
            action: "card:lock"
        });
        if (!multi) {
            actions.push({ separator: true, label: "" });
            actions.push({ label: "Connect one-way arrow…", icon: "arrow-right", action: "card:link-one" });
            actions.push({ label: "Connect two-way arrow…", icon: "arrow-up-down", action: "card:link-two" });
            if (outgoing > 0)
                actions.push({ label: "Clear outgoing arrows", icon: "eraser", action: "card:clear-links" });
            if (incoming > 0)
                actions.push({ label: "Clear incoming arrows", icon: "eraser", action: "card:clear-incoming" });
            actions.push({ separator: true, label: "" });
        }
        if (multi)
            actions.push({ label: "Create group", icon: "group", action: "card:group" });
        if (allGrouped) {
            actions.push({ label: "Rename group", icon: "square-pen", action: "group:rename" });
            actions.push({ label: "Delete group (keep cards)", icon: "ungroup", action: "group:delete" });
            actions.push({ label: multi ? "Ungroup cards" : "Ungroup", icon: "ungroup", action: "card:ungroup" });
        }
        if (allSynced) {
            actions.push({ label: multi ? "Stop syncing cards" : "Stop syncing", icon: "unlink", action: "card:unsync" });
        } else if (!multi) {
            var syncTargets = [];
            for (var t = 0; t < allCards.length; t++) {
                var other = allCards[t];
                if (other.id === card.id || other.type !== card.type) continue;
                syncTargets.push({
                    label: (other.title || other.type) + " · " + (other.gridName || "grid"),
                    icon: "link",
                    action: "card:sync:" + other.id
                });
            }
            if (syncTargets.length > 0)
                actions.push({ label: "Sync with card", icon: "link", children: syncTargets });
        }
        actions.push({
            label: multi ? "Save cards as template" : "Save as template",
            icon: "layout-template",
            action: "card:template"
        });
        if (!multi && card.url)
            actions.push({ label: "Open link", icon: "external-link", action: "card:open" });
        actions.push({ separator: true, label: "" });
        var sendTargets = [];
        for (var i = 0; i < grids.length; i++) {
            if (grids[i].id === app.activeGridId) continue;
            sendTargets.push({ label: grids[i].name, icon: "arrow-right", action: "card:send:" + grids[i].id });
        }
        if (sendTargets.length > 0)
            actions.push({ label: multi ? "Send cards to grid" : "Send to grid", icon: "arrow-right", children: sendTargets });
        actions.push({ separator: true, label: "" });
        actions.push({
            label: multi ? "Delete cards" : "Delete card",
            icon: "trash-2",
            action: "card:delete",
            danger: true
        });
        showMenu(px, py, actions);
    }

    function openGroupMenu(groupId, name, px, py) {
        root.menuGroup = { id: groupId, name: name || "Group", incoming: 0 };
        var target = "group:" + groupId;
        for (var i = 0; i < allCards.length; i++) {
            var links = allCards[i].links || [];
            for (var l = 0; l < links.length; l++)
                if (links[l].to === target) root.menuGroup.incoming++;
        }
        var actions = [{ label: "Rename group", icon: "square-pen", action: "group:rename" }];
        if (root.menuGroup.incoming > 0)
            actions.push({ label: "Clear arrows to group", icon: "eraser", action: "group:clear-incoming" });
        actions.push({ label: "Delete group (keep cards)", icon: "ungroup", action: "group:delete" });
        showMenu(px, py, actions);
    }

    function openGridMenu(grid, px, py) {
        menuGrid = grid;
        var actions = [
            { label: "Rename grid", icon: "square-pen", action: "grid:rename" },
            { label: "Choose icon…", icon: "image", action: "grid:icon" },
            { label: "Use default icon", icon: "layout-grid", action: "grid:icon-reset" }
        ];
        for (var i = 0; i < folders.length; i++)
            actions.push({ label: "Move to " + folders[i].name, icon: "folder-plus", action: "grid:move:" + folders[i].id });
        actions.push({ label: "Move out of folder", icon: "folder", action: "grid:move:" });
        actions.push({ separator: true, label: "" });
        actions.push({ label: "Delete grid", icon: "trash-2", action: "grid:delete", danger: true });
        showMenu(px, py, actions);
    }

    function openFolderMenu(folder, px, py) {
        menuFolder = folder;
        showMenu(px, py, [
            { label: "Rename folder", icon: "square-pen", action: "folder:rename" },
            { label: "Remove folder, keep grids", icon: "folder", action: "folder:delete-keep" },
            { separator: true, label: "" },
            { label: "Remove folder and grids", icon: "trash-2", action: "folder:delete-all", danger: true }
        ]);
    }

    function runMenuAction(action) {
        var parts = action.split(":");
        var kind = parts[0];
        var arg = parts.length > 1 ? parts.slice(1).join(":") : "";

        if (kind === "create") {
            root.createCard(arg, menuWorld.x, menuWorld.y);
            return;
        }
        if (action === "pick-file") { pinFileDialog.open(); return; }
        if (action === "pick-folder") { pinFolderDialog.open(); return; }

        if (kind === "card" && menuCard) {
            var ids = root.menuCardIds.length > 0 ? root.menuCardIds.slice() : [menuCard.id];
            var selectedCards = [];
            for (var i = 0; i < ids.length; i++) {
                var card = canvas.cardById(ids[i]);
                if (card) selectedCards.push(card);
            }
            if (arg === "duplicate") {
                for (var d = 0; d < ids.length; d++) app.duplicateCard(ids[d]);
            } else if (arg === "pip") {
                root.openFloatingCards(ids);
                canvas.setSelection([]);
            } else if (arg === "unpip") {
                for (var unpip = 0; unpip < ids.length; unpip++) root.closeFloatingCard(ids[unpip]);
            } else if (arg === "color") {
                var colors = ["#d98e5c", "#c9a86a", "#7aa2f7", "#86b78a", "#5aa8a0", "#d97a72", "#8d8d97"];
                for (var c = 0; c < selectedCards.length; c++) {
                    var at = colors.indexOf(selectedCards[c].color);
                    app.updateCard(JSON.stringify({
                        id: selectedCards[c].id,
                        color: colors[(at + 1) % colors.length]
                    }));
                }
            } else if (arg === "link-date") {
                dateLinkDialog.openFor(menuCard.id, "", "", "");
            } else if (arg === "lock") {
                for (var l = 0; l < ids.length; l++)
                    app.updateCard(JSON.stringify({ id: ids[l], locked: root.menuLockTarget }));
            } else if (arg === "link-one") {
                canvas.beginCardLink(menuCard.id, false);
            } else if (arg === "link-two") {
                canvas.beginCardLink(menuCard.id, true);
            } else if (arg === "clear-links") {
                app.clearCardLinks(menuCard.id);
            } else if (arg === "clear-incoming") {
                app.clearIncomingLinks(menuCard.id);
            } else if (arg === "group") {
                root.pendingGroupIds = ids.slice();
                root.pendingGroupId = "g" + Date.now();
                root.promptKind = "group-create";
                prompt.open("Name group", "Group", "Group name");
            } else if (arg === "ungroup") {
                app.setCardGroup(JSON.stringify(ids), "");
            } else if (arg === "unsync") {
                for (var u = 0; u < selectedCards.length; u++)
                    if (selectedCards[u].syncId) app.unsyncCard(selectedCards[u].id);
            } else if (arg.indexOf("sync:") === 0) {
                if (!app.syncCards(menuCard.id, arg.slice(5)))
                    toastHost.show("error", "Cards must be the same type");
            } else if (arg === "template") {
                root.templateTarget = ids.slice();
                root.promptKind = "template";
                prompt.open("Save as template", "", "Template name");
            } else if (arg === "open") {
                app.openExternal(menuCard.url);
            } else if (arg === "delete") {
                for (var del = 0; del < ids.length; del++) app.deleteCard(ids[del]);
                canvas.setSelection([]);
            } else if (arg.indexOf("send:") === 0) {
                var gridId = arg.slice(5);
                for (var send = 0; send < ids.length; send++) app.sendCardToGrid(ids[send], gridId);
                canvas.setSelection([]);
            }
            return;
        }

        if (kind === "group" && menuGroup) {
            if (arg === "rename") {
                root.pendingGroupId = menuGroup.id;
                root.promptKind = "group-rename";
                prompt.open("Rename group", menuGroup.name, "Group name");
            } else if (arg === "delete") {
                app.deleteCardGroup(menuGroup.id);
            } else if (arg === "clear-incoming") {
                app.clearIncomingLinks("group:" + menuGroup.id);
            }
            return;
        }

        if (kind === "grid" && menuGrid) {
            if (arg === "rename") {
                renameTarget = { kind: "grid", id: menuGrid.id };
                prompt.open("Rename grid", menuGrid.name, "Grid name");
            } else if (arg === "icon") {
                gridIconDialog.open();
            } else if (arg === "icon-reset") {
                app.setGridIcon(menuGrid.id, "layout-grid");
            } else if (arg === "delete") {
                app.deleteGrid(menuGrid.id);
            } else if (arg.indexOf("move:") === 0) {
                app.moveGridToFolder(menuGrid.id, arg.slice(5));
            }
            return;
        }

        if (kind === "folder" && menuFolder) {
            if (arg === "rename") {
                renameTarget = { kind: "folder", id: menuFolder.id };
                prompt.open("Rename folder", menuFolder.name, "Folder name");
            } else if (arg === "delete-keep") {
                app.deleteFolder(menuFolder.id, true);
            } else if (arg === "delete-all") {
                app.deleteFolder(menuFolder.id, false);
            }
        }
    }

    function zoomBy(factor) {
        var next = Math.min(3.0, Math.max(0.2, app.zoom * factor));
        var cx = canvas.width / 2;
        var cy = canvas.height / 2;
        var wx = (cx - app.panX) / app.zoom;
        var wy = (cy - app.panY) / app.zoom;
        app.setView(cx - wx * next, cy - wy * next, next);
        app.saveNow();
    }

    Connections {
        target: app
        function onToast(message) { toastHost.show("info", message); }
        function onNotice(kind, message) { toastHost.show(kind, message); }
        function onSettingsJsonChanged() {
            root.refreshAppearance();
            if (!onboardingDismissed && root.settings && root.settings.onboarding_completed === false && root.visible)
                onboarding.open();
        }
        function onThemesJsonChanged() { root.refreshAppearance() }
        function onFontsJsonChanged() { root.refreshAppearance() }
    }

    Component.onCompleted: {
        app.boot();
        root.refreshAppearance();
        onboardingTimer.start();
    }

    Timer {
        id: onboardingTimer
        interval: 250
        onTriggered: {
            if (!onboardingDismissed && root.settings && root.settings.onboarding_completed === false)
                onboarding.open();
        }
    }

    Timer {
        interval: 1500
        running: true
        repeat: true
        onTriggered: app.reloadIfChanged()
    }

    Shortcut { sequences: ["Ctrl+K"]; onActivated: launcher.open() }
    Shortcut { sequences: ["Ctrl+,"]; onActivated: root.openSettings("general") }
    Shortcut { sequences: ["Ctrl+Z"]; onActivated: app.undo() }
    Shortcut { sequence: "Ctrl+Shift+Z"; onActivated: app.redo() }
    Shortcut { sequence: "Ctrl+Y"; onActivated: app.redo() }
    Shortcut { sequences: ["Ctrl+0"]; onActivated: root.zoomBy(1.0 / app.zoom) }
    Shortcut {
        sequence: "Escape"
        onActivated: {
            if (canvas.linkSourceId !== "") canvas.cancelCardLink();
            else if (irohWizard.opened) irohWizard.close();
            else if (dateLinkDialog.opened) dateLinkDialog.close();
            else if (calendarManager.opened) calendarManager.close();
            else if (prompt.opened) prompt.close();
            else if (contextMenu.visible) contextMenu.close();
            else if (launcher.opened) launcher.close();
            else if (settingsDialog.opened) settingsDialog.closed();
            else if (onboarding.opened) {
                root.onboardingDismissed = true;
                onboarding.opened = false;
                app.setSetting("onboarding_completed", "true");
            } else root.settingsOpen = false;
        }
    }

    Row {
        anchors.fill: parent

        Sidebar {
            id: sidebar
            height: parent.height
            app: app
            th: root.th
            settings: root.settings
            grids: root.grids
            folders: root.folders
            activeGridId: app.activeGridId
            collapsed: root.sidebarCollapsed || root.width < 800
            onGridSelected: function (id) { app.switchGrid(id) }
            onNewGridRequested: app.createGrid("New Grid")
            onNewFolderRequested: app.createFolder("New Folder")
            onTemplatesRequested: templatesDialog.open()
            onSettingsRequested: root.openSettings("general")
            onToggleCollapsed: root.sidebarCollapsed = !root.sidebarCollapsed
            onGridMenuRequested: function (grid, x, y) { root.openGridMenu(grid, x, y) }
            onFolderMenuRequested: function (folder, x, y) { root.openFolderMenu(folder, x, y) }
        }

        Column {
            width: parent.width - sidebar.width
            height: parent.height

            Rectangle {
                id: topbar
                width: parent.width
                height: root.th.topbarHeight
                color: root.th.bg

                Rectangle {
                    anchors.bottom: parent.bottom
                    width: parent.width
                    height: root.th.borderWidth
                    color: root.th.border
                }

                Row {
                    anchors.left: parent.left
                    anchors.leftMargin: root.th.space5
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: root.th.space3
                    TextInput {
                        id: gridTitle
                        width: Math.min(360, Math.max(80, contentWidth + 8))
                        text: root.activeGrid ? root.activeGrid.name : ""
                        color: root.th.text
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizePage
                        font.weight: root.th.fontWeightMedium
                        selectionColor: root.th.accent
                        onEditingFinished: {
                            if (root.activeGrid && text.trim() !== "") app.renameGrid(root.activeGrid.id, text.trim());
                        }
                    }

                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        visible: root.width > 860
                        text: root.cards.length + (root.cards.length === 1 ? " card" : " cards")
                        color: root.th.muted
                        font.family: root.th.fontMono
                        font.pixelSize: root.th.fontSizeSmall
                    }
                }

                Row {
                    anchors.right: parent.right
                    anchors.rightMargin: root.th.space5
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: root.th.space2
                    AppButton {
                        th: root.th
                        icon: "search"
                        text: root.width > 1180 ? "Launcher" : ""
                        onClicked: launcher.open()
                    }

                    Item { width: 6; height: 1; visible: root.width > 980 }

                    Icon {
                        visible: root.width > 980
                        anchors.verticalCenter: parent.verticalCenter
                        name: "zoom-out"
                        size: root.th.iconBody
                        color: root.th.text2
                        TapHandler { onTapped: root.zoomBy(0.9) }
                    }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        visible: root.width > 980
                        text: Math.round(app.zoom * 100) + "%"
                        color: root.th.text2
                        font.family: root.th.fontMono
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Icon {
                        anchors.verticalCenter: parent.verticalCenter
                        visible: root.width > 980
                        name: "zoom-in"
                        size: root.th.iconBody
                        color: root.th.text2
                        TapHandler { onTapped: root.zoomBy(1.1) }
                    }
                    Icon {
                        anchors.verticalCenter: parent.verticalCenter
                        visible: root.width > 980
                        name: "maximize"
                        size: root.th.iconBody
                        color: root.th.muted
                        TapHandler { onTapped: root.zoomBy(1.0 / app.zoom) }
                    }

                    Item { width: 6; height: 1 }

                    AppButton {
                        th: root.th
                        icon: "plus"
                        text: "Create"
                        variant: "solid"
                        onClicked: {
                            var p = mapToItem(null, width - 220, height + 6);
                            root.openCreateMenu(p.x, p.y,
                                (canvas.width / 2 - app.panX) / app.zoom,
                                (canvas.height / 2 - app.panY) / app.zoom);
                        }
                    }
                }
            }

            CanvasView {
                id: canvas
                width: parent.width
                height: parent.height - topbar.height - statusbar.height
                app: app
                th: root.th
                settings: root.settings
                cards: root.cards
                floatingIds: root.floatingIds
                modalOpen: root.modalOpen
                onMenuRequested: function (card, x, y) { root.openCardMenu(card, x, y) }
                onGroupMenuRequested: function (groupId, name, x, y) { root.openGroupMenu(groupId, name, x, y) }
                onDateLinkRequested: function (cardId, itemId, linkId, date) {
                    dateLinkDialog.openFor(cardId, itemId, linkId, date);
                }
                onCalendarManagerRequested: calendarManager.open()
                onReturnCardRequested: function (id) { root.closeFloatingCard(id) }
                onBackgroundMenuRequested: function (gx, gy, wx, wy) { root.openCanvasMenu(gx, gy, wx, wy) }
                onCreateRequested: function (wx, wy) { root.createCard("note", wx, wy) }
            }

            Rectangle {
                id: statusbar
                width: parent.width
                height: root.th.statusbarHeight
                color: root.th.bg

                Rectangle {
                    anchors.top: parent.top
                    width: parent.width
                    height: root.th.borderWidth
                    color: root.th.border
                }

                Row {
                    anchors.left: parent.left
                    anchors.leftMargin: root.th.space5
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: root.th.space2
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Double-click to create"
                        color: root.th.muted
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Keycap { th: root.th; text: "Ctrl K" }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Launcher"
                        color: root.th.muted
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Keycap { th: root.th; text: "Ctrl Z" }
                    Text {
                        anchors.verticalCenter: parent.verticalCenter
                        text: "Undo"
                        color: root.th.muted
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                }

                Row {
                    anchors.right: parent.right
                    anchors.rightMargin: root.th.space5
                    anchors.verticalCenter: parent.verticalCenter
                    spacing: root.th.spaceSnug
                    Text {
                        text: app.syncing ? "Syncing" : ""
                        color: root.th.text2
                        font.family: root.th.fontFamily
                        font.pixelSize: root.th.fontSizeSmall
                    }
                    Text {
                        text: "v" + app.version
                        color: root.th.muted
                        font.family: root.th.fontMono
                        font.pixelSize: root.th.fontSizeSmall
                    }
                }
            }
        }
    }

    FileDialog {
        id: pinFileDialog
        title: "Pin an image, PDF or Markdown file"
        nameFilters: [
            "Images (*.png *.jpg *.jpeg *.webp *.gif *.bmp *.svg)",
            "PDF (*.pdf)",
            "Markdown (*.md *.markdown)"
        ]
        onAccepted: {
            var result = String(app.pinPath(root.filePath(selectedFile), menuWorld.x, menuWorld.y, ""));
            if (result.indexOf("error:") === 0) app.toast(result.slice(6));
        }
    }

    FolderDialog {
        id: pinFolderDialog
        title: "Pin a folder"
        onAccepted: {
            var result = String(app.pinPath(root.filePath(selectedFolder), menuWorld.x, menuWorld.y, ""));
            if (result.indexOf("error:") === 0) app.toast(result.slice(6));
        }
    }

    FileDialog {
        id: gridIconDialog
        title: "Choose a grid icon"
        nameFilters: ["SVG images (*.svg)"]
        onAccepted: if (menuGrid) app.setGridIcon(menuGrid.id, root.filePath(selectedFile))
    }

    ContextMenu {
        id: contextMenu
        parent: root.contentItem
        th: root.th
        actions: root.menuActions
        onTriggered: function (action) { root.runMenuAction(action) }
    }

    PromptDialog {
        id: prompt
        parent: root.contentItem
        th: root.th
        onAccepted: function (text) {
            if (text.trim() === "") return;
            if (root.promptKind === "template") {
                var raw = String(app.createTemplate(text.trim(), JSON.stringify(root.templateTarget)));
                root.promptKind = "";
                if (raw.indexOf("error:") === 0) toastHost.show("error", raw.slice(6));
                return;
            }
            if (root.promptKind === "group-create") {
                var groupId = root.pendingGroupId;
                var groupIds = root.pendingGroupIds.slice();
                root.promptKind = "";
                app.setCardGroup(JSON.stringify(groupIds), groupId);
                app.renameCardGroup(groupId, text.trim());
                root.pendingGroupIds = [];
                root.pendingGroupId = "";
                return;
            }
            if (root.promptKind === "group-rename") {
                var renameGroupId = root.pendingGroupId;
                root.promptKind = "";
                root.pendingGroupId = "";
                app.renameCardGroup(renameGroupId, text.trim());
                return;
            }
            if (root.renameTarget.kind === "grid") app.renameGrid(root.renameTarget.id, text.trim());
            else if (root.renameTarget.kind === "folder") app.renameFolder(root.renameTarget.id, text.trim());
        }
        onCancelled: {
            root.promptKind = "";
            root.pendingGroupIds = [];
            root.pendingGroupId = "";
        }
    }

    DateLinkDialog {
        id: dateLinkDialog
        parent: root.contentItem
        th: root.th
        app: app
    }

    CalendarManagerDialog {
        id: calendarManager
        parent: root.contentItem
        th: root.th
        app: app
    }

    Launcher {
        id: launcher
        app: app
        th: root.th
        onSettingsRequested: root.openSettings("general")
        onCardCreated: function (id) { canvas.focusCard(id) }
    }

    IrohSyncWizard {
        id: irohWizard
        app: app
        th: root.th
    }

    TemplatesDialog {
        id: templatesDialog
        parent: root.contentItem
        app: app
        th: root.th
    }

    SettingsDialog {
        id: settingsDialog
        app: app
        th: root.th
        opened: root.settingsOpen
        tab: root.settingsTab
        onClosed: root.settingsOpen = false
        onIrohWizardRequested: irohWizard.open()
    }

    Onboarding {
        id: onboarding
        app: app
        th: root.th
        onStartExploring: {
            root.onboardingDismissed = true;
            onboarding.opened = false;
            app.setSetting("onboarding_completed", "true");
        }
        onOpenBackup: {
            root.onboardingDismissed = true;
            onboarding.opened = false;
            app.setSetting("onboarding_completed", "true");
            root.openSettings("data");
        }
    }

    ToastHost {
        id: toastHost
        th: root.th
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.rightMargin: 20
        anchors.bottomMargin: 40
    }
}
