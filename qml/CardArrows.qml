import QtQuick

// Purely visual connectors between cards and groups. The source card stores
// its outgoing links in its payload; this item resolves both endpoints and
// paints an arrow. It never handles input.
Item {
    id: root

    property var cards: []
    property var groups: []
    property var th

    function center(rect) {
        return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
    }

    // Point where a line from the rectangle centre towards `other` leaves the
    // rectangle. Lines therefore end on the card edge, not under the card.
    function edgePoint(rect, other) {
        var c = center(rect);
        var dx = other.x - c.x;
        var dy = other.y - c.y;
        if (dx === 0 && dy === 0) return c;
        var tx = dx === 0 ? Infinity : (rect.width / 2) / Math.abs(dx);
        var ty = dy === 0 ? Infinity : (rect.height / 2) / Math.abs(dy);
        var t = Math.min(tx, ty);
        return { x: c.x + dx * t, y: c.y + dy * t };
    }

    readonly property var legendStyles: {
        var styles = {};
        var list = root.cards || [];
        for (var i = 0; i < list.length; i++) {
            var card = list[i];
            if (card.type !== "legend") continue;
            var entries = card.entries || [];
            for (var e = 0; e < entries.length; e++) {
                var entry = entries[e];
                if (entry && entry.id) styles[entry.id] = entry;
            }
        }
        return styles;
    }

    readonly property var arrows: {
        var list = root.cards || [];
        var groupList = root.groups || [];
        var byGroup = {};
        for (var g = 0; g < groupList.length; g++) byGroup[groupList[g].id] = groupList[g];
        var out = [];
        var fallback = (root.th && root.th.accent) ? root.th.accent : "#d98e5c";

        for (var i = 0; i < list.length; i++) {
            var card = list[i];
            var links = card.links || [];
            if (!card || !links.length) continue;
            var sourceRect = { x: card.x, y: card.y, width: card.width, height: card.height };
            for (var l = 0; l < links.length; l++) {
                var link = links[l];
                if (!link || !link.to) continue;
                var targetRect = null;
                if (link.to.indexOf("group:") === 0) {
                    targetRect = byGroup[link.to.slice(6)] || null;
                } else {
                    for (var t = 0; t < list.length; t++) {
                        if (list[t].id === link.to) {
                            targetRect = {
                                x: list[t].x,
                                y: list[t].y,
                                width: list[t].width,
                                height: list[t].height
                            };
                            break;
                        }
                    }
                }
                if (!targetRect || targetRect === sourceRect) continue;
                if (sourceRect.x === targetRect.x && sourceRect.y === targetRect.y
                        && sourceRect.width === targetRect.width
                        && sourceRect.height === targetRect.height)
                    continue;
                var s = edgePoint(sourceRect, center(targetRect));
                var e = edgePoint(targetRect, center(sourceRect));
                if (Math.abs(s.x - e.x) < 4 && Math.abs(s.y - e.y) < 4) continue;
                var style = root.legendStyles[link.style] || null;
                out.push({
                    key: card.id + "|" + link.to,
                    sx: s.x,
                    sy: s.y,
                    tx: e.x,
                    ty: e.y,
                    double: link.double === true,
                    color: style && style.color ? style.color : fallback,
                    dashed: !!(style && style.dashed)
                });
            }
        }
        return out;
    }

    Repeater {
        model: root.arrows

        delegate: Canvas {
            required property var modelData

            x: Math.min(modelData.sx, modelData.tx) - 14
            y: Math.min(modelData.sy, modelData.ty) - 14
            width: Math.max(4, Math.abs(modelData.sx - modelData.tx) + 28)
            height: Math.max(4, Math.abs(modelData.sy - modelData.ty) + 28)

            onModelDataChanged: requestPaint()
            onWidthChanged: requestPaint()
            onHeightChanged: requestPaint()
            Component.onCompleted: requestPaint()

            onPaint: {
                var ctx = getContext("2d");
                ctx.clearRect(0, 0, width, height);
                var sx = modelData.sx - x;
                var sy = modelData.sy - y;
                var tx = modelData.tx - x;
                var ty = modelData.ty - y;
                var angle = Math.atan2(ty - sy, tx - sx);

                ctx.strokeStyle = modelData.color;
                ctx.fillStyle = modelData.color;
                ctx.lineWidth = 2;
                ctx.setLineDash(modelData.dashed ? [7, 5] : []);
                ctx.beginPath();
                ctx.moveTo(sx, sy);
                ctx.lineTo(tx, ty);
                ctx.stroke();
                ctx.setLineDash([]);

                function head(hx, hy, direction) {
                    var size = 10;
                    var half = 5;
                    ctx.beginPath();
                    ctx.moveTo(hx, hy);
                    ctx.lineTo(hx - size * Math.cos(direction) + half * Math.sin(direction),
                               hy - size * Math.sin(direction) - half * Math.cos(direction));
                    ctx.lineTo(hx - size * Math.cos(direction) - half * Math.sin(direction),
                               hy - size * Math.sin(direction) + half * Math.cos(direction));
                    ctx.closePath();
                    ctx.fill();
                }

                head(tx, ty, angle);
                if (modelData.double) head(sx, sy, angle + Math.PI);
            }
        }
    }
}
