(() => {
  try {
    if (!document.documentElement) return {pending:true};
    const copy = document.documentElement.cloneNode(true);
    copy.querySelectorAll("script,style,template,noscript,svg,canvas,iframe").forEach(node => node.remove());
    const html = copy.outerHTML;
    const encoded = new TextEncoder().encode(html);
    const maximum = __MAX_BYTES__;
    let end = Math.min(encoded.length, maximum);
    // Do not introduce a replacement character at a split UTF-8 boundary.
    if (end < encoded.length) while (end > 0 && (encoded[end] & 0xc0) === 0x80) end--;
    return {url:location.href,html:new TextDecoder().decode(encoded.subarray(0,end)),
      truncated:encoded.length>maximum,
      settled:document.readyState === "complete" && window.__aworkitDomQuiet?.() >= 500 &&
        !/^loading[. …]*$/i.test(document.body?.innerText.trim() ?? "")};
  } catch (error) { return {error:String(error).slice(0,512)}; }
})();
