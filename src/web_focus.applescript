const targetUrl = "{url}";
function normalize(value) {
  value = String(value || '').replace('http://localhost:', 'http://127.0.0.1:');
  value = value.split('?')[0].split('#')[0];
  return value.endsWith('/') ? value : value + '/';
}
for (const name of ['Google Chrome', 'Microsoft Edge', 'Brave Browser']) {
  try {
    const app = Application(name);
    if (!app.running()) continue;
    for (const window of app.windows()) for (let index = 0; index < window.tabs().length; index++) {
      if (normalize(window.tabs()[index].url()) === normalize(targetUrl)) {
        window.activeTabIndex = index + 1; window.index = 1; app.activate(); name;
      }
    }
  } catch (_) {}
}
try {
  const safari = Application('Safari');
  if (safari.running()) for (const window of safari.windows()) for (const tab of window.tabs()) {
    if (normalize(tab.url()) === normalize(targetUrl)) { window.currentTab = tab; window.index = 1; safari.activate(); 'Safari'; }
  }
} catch (_) {}
'';
