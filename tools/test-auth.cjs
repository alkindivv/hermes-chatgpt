const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const script = fs.readFileSync(path.join(__dirname, '../crates/browser/assets/page.js'), 'utf8');

async function inspect(status, body, error) {
  const context = vm.createContext({
    document: { readyState: 'complete', querySelectorAll: () => [] },
    location: { origin: 'https://chatgpt.com' }, URL, AbortSignal,
    fetch: async () => {
      if (error) throw error;
      return { status, ok: status >= 200 && status < 300,
        url: 'https://chatgpt.com/api/auth/session', json: async () => body };
    },
  });
  return JSON.parse(JSON.stringify(await vm.runInContext(`${script}; page('auth', null)`, context)));
}
(async () => {
  for (const status of [200, 401]) {
    const result = await inspect(status, {});
    assert.equal(result.server_authenticated, false);
    assert.equal(result.signed_out_confirmed, true, `signed-out ${status}`);
  }
  for (const [status, body, error] of [
    [503, {}, undefined], [200, {}, new Error('network unavailable')],
    [200, { error: 'RefreshAccessTokenError' }, undefined],
    [200, { user: { id: 'fixture' }, expires: '2000-01-01T00:00:00Z' }, undefined],
    [200, null, undefined],
  ]) {
    const result = await inspect(status, body, error);
    assert.equal(result.server_authenticated, false);
    assert.equal(result.signed_out_confirmed, false, 'unknown must not become logout proof');
  }
  const result = await inspect(200, { user: { id: 'fixture' }, expires: '2999-01-01T00:00:00Z' });
  assert.equal(result.server_authenticated, true);
  assert.equal(result.signed_out_confirmed, false);
  console.log('auth evidence: 8 cases passed');
})().catch(error => { console.error(error.message); process.exitCode = 1; });
