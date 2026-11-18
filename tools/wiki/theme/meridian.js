// Meridian's additions to each page of the documentation site. See
// docs/contributing/documentation-site.md, "Motion".
//
// Everything here is an enhancement: without this script the page is the
// same page, still, with the still mark drawn by theme/meridian.css. A reader
// whose system asks for less motion gets the sky without its travelling
// light, no reveals and no parallax; the stylesheet turns the rest off.
(function () {
    "use strict";

    var reduced = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    // mdBook sets path_to_root on every page; the icon link is the fallback.
    function root() {
        if (typeof path_to_root === "string") {
            return path_to_root;
        }
        var icon = document.querySelector('link[rel="icon"]');
        return icon ? icon.getAttribute("href").replace(/[^/]*$/, "") : "";
    }

    // The animated mark beside the title, and the pair as a link home.
    function mark() {
        var title = document.querySelector(".menu-title");
        if (!title || title.querySelector(".meridian-mark")) {
            return;
        }
        var img = document.createElement("img");
        img.className = "meridian-mark";
        img.alt = "";
        img.width = 28;
        img.height = 28;
        img.src = root() + "branding/logos/meridian-core-animated.svg";

        var home = document.createElement("a");
        home.className = "meridian-home";
        home.href = root() + "index.html";
        home.setAttribute("aria-label", "Meridian documentation home");
        home.appendChild(img);
        while (title.firstChild) {
            home.appendChild(title.firstChild);
        }
        title.appendChild(home);
        title.classList.add("has-mark");
    }

    // The sky: meridians drawn between two poles behind the whole page, a
    // faint grid of points, three slow glows, and light travelling along
    // four of the lines. It is inline SVG so the stylesheet can colour it for
    // each theme.
    function sky() {
        if (document.querySelector(".meridian-sky")) {
            return;
        }
        var ns = "http://www.w3.org/2000/svg";
        var W = 1600, H = 1000;
        var ax = 1460, ay = 120, bx = 140, by = 1040;  // the two poles
        var dx = bx - ax, dy = by - ay;
        var half = Math.sqrt(dx * dx + dy * dy) / 2;
        var angle = Math.atan2(dy, dx) * 180 / Math.PI;
        var categories = ["iac", "cm", "containers", "orchestration"];

        function el(name, attrs) {
            var node = document.createElementNS(ns, name);
            for (var key in attrs) {
                node.setAttribute(key, attrs[key]);
            }
            return node;
        }

        // An elliptical arc from pole A to pole B whose minor radius is a
        // share of the major one; 0 is the straight line between the poles.
        function meridian(share, sweep) {
            var ry = Math.max(half * Math.abs(share), 1);
            return "M" + ax + " " + ay + "A" + half + " " + ry + " " + angle + " 0 " + sweep + " " + bx + " " + by;
        }

        var svg = el("svg", {
            viewBox: "0 0 " + W + " " + H,
            preserveAspectRatio: "xMidYMid slice",
            "aria-hidden": "true",
            focusable: "false",
        });

        var defs = el("defs", {});
        var pattern = el("pattern", { id: "meridian-sky-grid", width: 36, height: 36, patternUnits: "userSpaceOnUse" });
        pattern.appendChild(el("circle", { cx: 18, cy: 18, r: 1.1, class: "meridian-sky-dot" }));
        defs.appendChild(pattern);
        categories.forEach(function (name) {
            var glow = el("radialGradient", { id: "meridian-sky-glow-" + name });
            glow.appendChild(el("stop", { offset: "0", class: "meridian-sky-glow-" + name, "stop-opacity": "1" }));
            glow.appendChild(el("stop", { offset: "1", class: "meridian-sky-glow-" + name, "stop-opacity": "0" }));
            defs.appendChild(glow);
        });
        svg.appendChild(defs);

        var world = el("g", { class: "meridian-sky-world" });
        world.appendChild(el("rect", { width: W, height: H, fill: "url(#meridian-sky-grid)" }));

        var glows = [["iac", 1250, 220, 420], ["cm", 420, 760, 380], ["orchestration", 1180, 860, 360]];
        glows.forEach(function (g, i) {
            world.appendChild(el("circle", {
                cx: g[1], cy: g[2], r: g[3],
                fill: "url(#meridian-sky-glow-" + g[0] + ")",
                class: "meridian-sky-glow meridian-sky-glow-" + (i + 1),
            }));
        });

        var shares = [-0.78, -0.56, -0.36, -0.18, 0, 0.18, 0.36, 0.56, 0.78];
        var lines = el("g", { class: "meridian-sky-lines" });
        shares.forEach(function (share) {
            lines.appendChild(el("path", { d: meridian(share, share < 0 ? 0 : 1) }));
        });
        world.appendChild(lines);

        // The light: four lines, one per category, each with a soft and a
        // sharp pulse, so the pulses leave the pole one after another.
        var lit = [[-0.56, "iac"], [-0.18, "cm"], [0.18, "containers"], [0.56, "orchestration"]];
        var light = el("g", { class: "meridian-sky-light" });
        lit.forEach(function (pair, i) {
            var d = meridian(pair[0], pair[0] < 0 ? 0 : 1);
            var delay = "animation-delay:" + (-i * 2.1) + "s";
            light.appendChild(el("path", { d: d, pathLength: 100, class: "meridian-sky-pulse soft " + pair[1], style: delay }));
            light.appendChild(el("path", { d: d, pathLength: 100, class: "meridian-sky-pulse sharp " + pair[1], style: delay }));
        });
        world.appendChild(light);

        world.appendChild(el("circle", { cx: ax, cy: ay, r: 7, class: "meridian-sky-pole" }));
        world.appendChild(el("circle", { cx: ax, cy: ay, r: 22, class: "meridian-sky-halo" }));
        svg.appendChild(world);

        var holder = document.createElement("div");
        holder.className = "meridian-sky";
        holder.setAttribute("aria-hidden", "true");
        holder.appendChild(svg);
        document.body.insertBefore(holder, document.body.firstChild);
        document.documentElement.classList.add("has-sky");

        // The sky leans a little towards the pointer, on devices that have one.
        if (!reduced && window.matchMedia && window.matchMedia("(pointer: fine)").matches) {
            var pending = false, px = 0, py = 0;
            window.addEventListener("mousemove", function (event) {
                px = (event.clientX / window.innerWidth - 0.5) * 2;
                py = (event.clientY / window.innerHeight - 0.5) * 2;
                if (!pending) {
                    pending = true;
                    window.requestAnimationFrame(function () {
                        world.style.transform = "translate(" + (px * -10).toFixed(1) + "px, " + (py * -8).toFixed(1) + "px)";
                        pending = false;
                    });
                }
            }, { passive: true });
        }
    }

    // A line of the four meridians across the top that fills as the page is read.
    function progress() {
        if (document.querySelector(".meridian-progress")) {
            return;
        }
        var bar = document.createElement("div");
        bar.className = "meridian-progress";
        bar.setAttribute("aria-hidden", "true");
        document.body.appendChild(bar);
        var pending = false;
        function update() {
            var doc = document.documentElement;
            var total = doc.scrollHeight - doc.clientHeight;
            var share = total > 0 ? Math.min(1, Math.max(0, (window.scrollY || doc.scrollTop) / total)) : 0;
            bar.style.transform = "scaleX(" + share.toFixed(4) + ")";
            pending = false;
        }
        window.addEventListener("scroll", function () {
            if (!pending) {
                pending = true;
                window.requestAnimationFrame(update);
            }
        }, { passive: true });
        window.addEventListener("resize", update);
        update();
    }

    // Each block of a page rises in as it scrolls into view. The classes are
    // added only here, right before the observer starts, so a page without
    // this script, or a browser without observers, shows everything at once.
    function reveal() {
        var main = document.querySelector(".content main");
        if (!main || reduced || !("IntersectionObserver" in window)) {
            return;
        }
        var blocks = Array.prototype.filter.call(main.children, function (node) {
            return !/^(H1|SCRIPT|STYLE)$/.test(node.tagName);
        });
        if (blocks.length < 3) {
            return;
        }
        var observer = new IntersectionObserver(function (entries) {
            entries.forEach(function (entry) {
                if (entry.isIntersecting) {
                    entry.target.classList.add("is-visible");
                    observer.unobserve(entry.target);
                }
            });
        }, { rootMargin: "0px 0px -8% 0px", threshold: 0.05 });
        blocks.forEach(function (node, i) {
            node.classList.add("meridian-reveal");
            // The first blocks are already in view; let them follow the page's own rise.
            node.style.transitionDelay = (Math.min(i, 6) * 0.05) + "s";
            observer.observe(node);
        });
        // A print, or a reader that jumps by anchor, must never find hidden text.
        window.addEventListener("beforeprint", function () {
            blocks.forEach(function (node) { node.classList.add("is-visible"); });
        });
        window.setTimeout(function () {
            blocks.forEach(function (node) {
                if (node.getBoundingClientRect().top < window.innerHeight) {
                    node.classList.add("is-visible");
                }
            });
        }, 1200);
    }

    mark();
    sky();
    progress();
    reveal();
})();
