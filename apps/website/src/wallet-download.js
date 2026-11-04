const repository = 'https://github.com/mmx-network/mmx-rust-node';
const releasesApi = 'https://api.github.com/repos/mmx-network/mmx-rust-node/releases';

/** Resolve wallet releases independently of the repository's latest node release. */
export async function latestWalletRelease(fetcher = fetch) {
  const signal = AbortSignal.timeout(5000);
  let latest = null;
  for (let page = 1; ; page++) {
    const response = await fetcher(`${releasesApi}?per_page=100&page=${page}`, {
      headers: { Accept: 'application/vnd.github+json' },
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
      redirect: 'error',
      signal,
    });
    if (!response.ok) throw new Error('Wallet releases unavailable');
    const releases = await response.json();
    if (!Array.isArray(releases)) throw new Error('Invalid wallet release list');
    for (const release of releases) {
      if (release?.draft !== false || release.prerelease !== false
        || !/^wallet-v[0-9][A-Za-z0-9.+-]*$/.test(release.tag_name)) continue;
      const published = Date.parse(release.published_at);
      if (Number.isFinite(published) && (!latest || published > latest.published)) {
        latest = { tag: release.tag_name, published };
      }
    }
    if (!response.headers.get('link')?.includes('rel="next"')) break;
  }
  return latest ? `${repository}/releases/tag/${encodeURIComponent(latest.tag)}` : null;
}

/** Fetch only on download intent; retain ordinary links when the API is unavailable. */
export function walletDownloadLinks(links, fetcher = fetch, navigate = (url) => window.location.assign(url)) {
  let lookup;
  function prepare() {
    return lookup ??= latestWalletRelease(fetcher).then((url) => {
      if (url) for (const link of links) link.href = url;
      return url;
    }).catch(() => null);
  }
  for (const link of links) {
    link.addEventListener('pointerenter', prepare, { once: true });
    link.addEventListener('focus', prepare, { once: true });
    link.addEventListener('click', async (event) => {
      if (event.defaultPrevented || event.button !== 0
        || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
      event.preventDefault();
      navigate(await prepare() ?? link.href);
    });
  }
}
