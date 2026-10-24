// Small client helpers: toasts, the drawer, history, keyboard shortcuts.

function toast(message) {
  const box = document.getElementById("toasts");
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = message;
  box.appendChild(el);
  setTimeout(() => el.remove(), 3600);
}

function openDrawer() {
  document.getElementById("drawer").hidden = false;
}

function closeDrawer() {
  const drawer = document.getElementById("drawer");
  drawer.hidden = true;
  drawer.innerHTML = "";
}

function toggleHistory() {
  const history = document.getElementById("history");
  history.hidden = !history.hidden;
  if (!history.hidden) htmx.trigger(history, "open");
}

// Server-sent toasts (HX-Trigger: {"toast": "..."}).
document.body.addEventListener("toast", (event) => toast(event.detail.value));

// Anything swapped into the drawer opens it.
document.body.addEventListener("htmx:afterSwap", (event) => {
  if (event.detail.target.id === "drawer") openDrawer();
});

document.body.addEventListener("htmx:responseError", (event) => {
  toast("Something went wrong — " + (event.detail.xhr.status || "network"));
});

document.addEventListener("keydown", (event) => {
  const typing = ["INPUT", "TEXTAREA", "SELECT"].includes(document.activeElement?.tagName);
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "z" && !typing) {
    event.preventDefault();
    htmx.ajax("POST", event.shiftKey ? "/api/redo" : "/api/undo", { swap: "none" });
  } else if (event.key === "Escape") {
    closeDrawer();
  }
  // Pages register their own keys (Spread editor).
  if (!typing && window.pageKeys) window.pageKeys(event);
});
