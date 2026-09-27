// A static export: `tenx web` embeds web/out in the binary and serves it, so
// the page has no server of its own — no SSR, no API routes, no rewrites.
/** @type {import('next').NextConfig} */
const nextConfig = {
  output: 'export',
  images: { unoptimized: true },
  reactStrictMode: true,
  devIndicators: false,
};

export default nextConfig;
