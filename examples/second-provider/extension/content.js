// Between the page's world and the service worker. The one thing decided here is user activation: the service worker
// cannot see it and the page's world can lie about it, while this world reads the browser's own answer.
(() => {
  let page;
  let worker = null;

  function toWorker() {
    if (worker !== null) return worker;
    worker = chrome.runtime.connect({ name: 'acp-page' });
    worker.onMessage.addListener((message) => page.postMessage(message));
    worker.onDisconnect.addListener(() => {
      worker = null;
      page.postMessage({ t: 'lost' });
    });
    return worker;
  }

  function hello(event) {
    if (event.source !== window || event.data?.acpProviderChannel === undefined || event.ports?.[0] === undefined) return;
    window.removeEventListener('message', hello);
    [page] = event.ports;
    page.onmessage = ({ data }) => {
      const message =
        data.t === 'call' && data.op === 'request' ? { ...data, activated: navigator.userActivation.isActive } : data;
      try {
        toWorker().postMessage(message);
      } catch {
        worker = null;
        page.postMessage({ t: 'lost' });
      }
    };
  }
  window.addEventListener('message', hello);

  // Ends the document's connections now rather than when the browser gets to it. A page restored from the back/forward
  // cache connects again on its next call.
  window.addEventListener('pagehide', () => {
    worker?.disconnect();
    worker = null;
  });
})();
