// Readiness is a heuristic, never evidence that an entire dynamic site was read.
(() => {
  let changed = Date.now();
  new MutationObserver(() => { changed = Date.now(); })
    .observe(document, {subtree:true, childList:true, characterData:true});
  Object.defineProperty(window, "__aworkitDomQuiet", {value:() => Date.now() - changed, configurable:false});
})();
