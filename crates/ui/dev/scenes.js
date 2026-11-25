// Dev only: puts the hub in a given state from the URL, for screenshots
// taken without clicking (headless Chrome, `just hub-shots`).
//   ?open=plugin:voice-agent   open that app's drawer
//   &tab=settings              on that tab
//   &edit=1                    make one unsaved settings change
//   &palette=call              open the palette with that text typed
(function () {
  const q = new URLSearchParams(location.search);
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  async function until(test, tries = 50) {
    for (let i = 0; i < tries; i++) {
      const found = test();
      if (found) return found;
      await wait(40);
    }
    return null;
  }
  async function run() {
    const open = q.get("open");
    if (open) {
      const card = await until(() => document.querySelector(`[data-card="${CSS.escape(open)}"]`));
      card?.click();
      const tab = q.get("tab");
      if (tab) {
        const button = await until(() =>
          [...document.querySelectorAll(".drawer .tabs button")].find((b) => b.textContent.toLowerCase().startsWith(tab)),
        );
        button?.click();
      }
      if (q.get("edit")) {
        // The last number field goes up by ten: one unsaved change.
        const fields = await until(() => {
          const all = document.querySelectorAll('.drawer .field-input input[type="number"]');
          return all.length ? all : null;
        });
        const input = fields?.[fields.length - 1];
        if (input) {
          input.value = String(Number(input.value) + 10);
          input.dispatchEvent(new Event("input", { bubbles: true }));
        }
        await wait(100);
      }
      document.activeElement?.blur();
    }
    const palette = q.get("palette");
    if (palette !== null) {
      await until(() => document.querySelector("#nav button"));
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "k", metaKey: true }));
      const input = await until(() => document.querySelector(".palette input"));
      if (input && palette) {
        input.value = palette;
        input.dispatchEvent(new Event("input"));
      }
    }
  }
  window.addEventListener("load", () => run());
})();
