// Where the server's HTTP endpoints live: this page's own origin when
// `tenx web` serves it, or `NEXT_PUBLIC_TENX_URL` under `next dev` — with the
// token in the query, since the cookie belongs to the other origin.

export function apiUrl(path: string): string {
  const dev = process.env.NEXT_PUBLIC_TENX_URL;
  if (!dev) return path;
  const url = new URL(path, dev);
  const token = process.env.NEXT_PUBLIC_TENX_TOKEN || new URLSearchParams(location.search).get('token');
  if (token) url.searchParams.set('token', token);
  return url.toString();
}
