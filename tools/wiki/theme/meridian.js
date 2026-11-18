// Puts the animated Meridian mark beside the site's title and makes the pair a
// link home. See docs/contributing/documentation-site.md. Without this script
// the still mark is drawn by theme/meridian.css instead, so nothing depends on
// it running.
(function () {
    "use strict";
    var title = document.querySelector(".menu-title");
    if (!title || title.querySelector(".meridian-mark")) {
        return;
    }
    // mdBook sets path_to_root on every page; the icon link is the fallback.
    var root = typeof path_to_root === "string" ? path_to_root : "";
    if (!root) {
        var icon = document.querySelector('link[rel="icon"]');
        if (icon) {
            root = icon.getAttribute("href").replace(/[^/]*$/, "");
        }
    }
    var mark = document.createElement("img");
    mark.className = "meridian-mark";
    mark.alt = "";
    mark.width = 28;
    mark.height = 28;
    mark.src = root + "branding/logos/meridian-core-animated.svg";

    var home = document.createElement("a");
    home.className = "meridian-home";
    home.href = root + "index.html";
    home.setAttribute("aria-label", "Meridian documentation home");
    home.appendChild(mark);
    while (title.firstChild) {
        home.appendChild(title.firstChild);
    }
    title.appendChild(home);
    title.classList.add("has-mark");
})();
