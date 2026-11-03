// Transist Inspect UI — tiny progressive sprinkles. UI works without this.
(function () {
  "use strict";

  // Copy-to-clipboard for any element with [data-copy].
  document.addEventListener("click", function (event) {
    var trigger = event.target.closest("[data-copy]");
    if (!trigger) return;
    var value = trigger.getAttribute("data-copy");
    if (!value || !navigator.clipboard) return;
    navigator.clipboard.writeText(value).then(
      function () {
        var prev = trigger.textContent;
        trigger.textContent = "copied";
        setTimeout(function () { trigger.textContent = prev; }, 1200);
      },
      function () { /* ignore */ }
    );
  });

  // <details> open-state persistence keyed by id.
  document.querySelectorAll("details[id]").forEach(function (el) {
    try {
      var key = "transist:details:" + el.id;
      var stored = localStorage.getItem(key);
      if (stored === "1") el.open = true;
      el.addEventListener("toggle", function () {
        try { localStorage.setItem(key, el.open ? "1" : "0"); } catch (_e) { /* ignore */ }
      });
    } catch (_e) { /* ignore */ }
  });

  // Auto-refresh helper. Triggered when a page renders <meta name="transist-auto-refresh" content="1">
  // and contains a #auto-refresh-toggle checkbox.
  var marker = document.querySelector('meta[name="transist-auto-refresh"]');
  var toggle = document.getElementById("auto-refresh-toggle");
  if (marker && toggle) {
    var KEY = "transist:auto-refresh";
    try {
      if (localStorage.getItem(KEY) === "1") toggle.checked = true;
    } catch (_e) { /* ignore */ }
    var interval = null;
    function tick() {
      if (document.hidden) return;
      window.location.reload();
    }
    function start() {
      if (interval) return;
      interval = setInterval(tick, 5000);
    }
    function stop() {
      if (!interval) return;
      clearInterval(interval);
      interval = null;
    }
    toggle.addEventListener("change", function () {
      try { localStorage.setItem(KEY, toggle.checked ? "1" : "0"); } catch (_e) { /* ignore */ }
      if (toggle.checked) start(); else stop();
    });
    if (toggle.checked) start();
  }
})();
