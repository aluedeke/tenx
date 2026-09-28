import type { Metadata, Viewport } from 'next';
import '@fontsource/jetbrains-mono/300.css';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@fontsource/jetbrains-mono/600.css';
import '@fontsource/jetbrains-mono/700.css';
import '../palette.css';
import './globals.css';
import { palette } from '@/palette';

export const metadata: Metadata = {
  title: 'tenx',
  description: 'Every task, its agent, and which one needs you.',
  icons: {
    icon: [
      { url: '/favicon.svg', type: 'image/svg+xml' },
      { url: '/favicon-32.png', sizes: '32x32', type: 'image/png' },
    ],
    apple: '/apple-touch-icon.png',
  },
  appleWebApp: { capable: true, title: 'tenx', statusBarStyle: 'black-translucent' },
};

export const viewport: Viewport = {
  width: 'device-width',
  initialScale: 1,
  // The page is a terminal: no pinch zoom, and the on-screen keyboard shrinks
  // the layout instead of covering it.
  maximumScale: 1,
  userScalable: false,
  interactiveWidget: 'resizes-content',
  // Draw under the notch and the home indicator; the page pads itself with
  // the safe-area insets.
  viewportFit: 'cover',
  themeColor: palette.GROUND,
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <head>
        {/* With the cookie: a signed-in page gets a manifest whose start URL
            logs the installed app in (its cookies are its own on iOS). */}
        <link rel="manifest" href="/manifest.webmanifest" crossOrigin="use-credentials" />
      </head>
      <body>{children}</body>
    </html>
  );
}
