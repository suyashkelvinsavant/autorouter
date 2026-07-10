// Inline monoline SVG icons. Drawing them inline avoids a font
// dependency and lets the icons inherit the current text colour.

import type { SVGProps } from "react";

const base: SVGProps<SVGSVGElement> = {
  width: 16,
  height: 16,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.6,
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
};

export function IconDashboard(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <rect x="3" y="3" width="7" height="9" rx="1.5" />
      <rect x="14" y="3" width="7" height="5" rx="1.5" />
      <rect x="14" y="12" width="7" height="9" rx="1.5" />
      <rect x="3" y="16" width="7" height="5" rx="1.5" />
    </svg>
  );
}

export function IconProviders(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.5 4.5l2 2M17.5 17.5l2 2M19.5 4.5l-2 2M6.5 17.5l-2 2" />
    </svg>
  );
}

export function IconSessions(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M4 6h16M4 12h10M4 18h16" />
      <circle cx="19" cy="12" r="1.6" />
    </svg>
  );
}

export function IconLogs(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M5 4h14a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1z" />
      <path d="M8 9h8M8 13h6M8 17h4" />
    </svg>
  );
}

export function IconSettings(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
    </svg>
  );
}

export function IconQuit(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
      <path d="M16 17l5-5-5-5" />
      <path d="M21 12H9" />
    </svg>
  );
}

export function IconSun(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41" />
    </svg>
  );
}

export function IconMoon(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M21 12.79A9 9 0 1 1 11.21 3a7 7 0 0 0 9.79 9.79z" />
    </svg>
  );
}

export function IconRoute(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <circle cx="6" cy="19" r="2" />
      <circle cx="18" cy="5" r="2" />
      <path d="M8 19h6a4 4 0 0 0 0-8H10a4 4 0 0 1 0-8h6" />
    </svg>
  );
}

export function IconHeart(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M3 12h4l3-9 4 18 3-9h4" />
    </svg>
  );
}

export function IconList(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01" />
    </svg>
  );
}

export function IconChart(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M3 3v18h18" />
      <path d="M7 14l4-4 3 3 6-7" />
    </svg>
  );
}

export function IconBug(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <rect x="8" y="7" width="8" height="13" rx="4" />
      <path d="M3 12h5M16 12h5M3 5l4 2M21 5l-4 2M9 2l1 2M15 2l-1 2M12 7v3" />
    </svg>
  );
}

export function IconWrench(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.3 2.3-2.7-2.7 2.3-2.3z" />
    </svg>
  );
}

export function IconBox(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M21 8L12 3 3 8v8l9 5 9-5V8z" />
      <path d="M3 8l9 5 9-5M12 13v8" />
    </svg>
  );
}

export function IconDownload(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M12 3v12" />
      <path d="M7 10l5 5 5-5" />
      <path d="M5 21h14" />
    </svg>
  );
}

export function IconUpload(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M12 21V9" />
      <path d="M7 14l5-5 5 5" />
      <path d="M5 3h14" />
    </svg>
  );
}

export function IconRefresh(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M3 12a9 9 0 0 1 15.5-6.3L21 8" />
      <path d="M21 3v5h-5" />
      <path d="M21 12a9 9 0 0 1-15.5 6.3L3 16" />
      <path d="M3 21v-5h5" />
    </svg>
  );
}

export function IconPower(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M12 3v9" />
      <path d="M5.5 8a8 8 0 1 0 13 0" />
    </svg>
  );
}

export function IconFolder(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
    </svg>
  );
}

export function IconCopy(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M5 15V5a2 2 0 0 1 2-2h10" />
    </svg>
  );
}

export function IconPlus(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M12 5v14M5 12h14" />
    </svg>
  );
}

export function IconTrash(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M3 6h18" />
      <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
      <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
    </svg>
  );
}

export function IconPlay(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M5 3l16 9-16 9V3z" />
    </svg>
  );
}

export function IconPause(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <rect x="6" y="4" width="4" height="16" rx="1" />
      <rect x="14" y="4" width="4" height="16" rx="1" />
    </svg>
  );
}

export function IconExternal(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M14 4h6v6" />
      <path d="M20 4L10 14" />
      <path d="M20 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h5" />
    </svg>
  );
}

export function IconPlug(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M12 2v6M5 10h14M5 14h14M12 18v6M8 5l-3 3M16 5l3 3M8 19l-3 3M16 19l3 3" />
    </svg>
  );
}

export function BrandMark(p: SVGProps<SVGSVGElement>) {
  return (
    <svg
      width={22}
      height={22}
      viewBox="0 0 271.3 219.27"
      fill="currentColor"
      aria-hidden
      {...p}
    >
      <path d="M169.61,16.99c-15,0-29.5,0-43.99,.01-9.95,0-17.3,4.1-22.72,12.74-26.9,42.91-54.13,85.61-81.09,128.48-10.68,16.99-2.63,37.72,16.24,42.51,1.75,.44,3.62,.58,5.43,.59,24.16,.04,48.33,.06,72.49,.03,8.38-.01,11.54-5.09,7.11-11.95-10.75-16.66-21.77-33.13-32.68-49.69-.37-.56-.64-1.19-.81-1.53-2.88,.89-3.61,2.91-4.66,4.51-5.37,8.24-10.63,16.54-15.91,24.83-1.61,2.52-2.89,5.08-3.47,8.16-1.05,5.56-6.08,9.25-11.55,9.18-4.72-.07-9.3-3.53-10.46-7.9-1.42-5.38,.83-10.99,5.93-13.52,2.86-1.42,4.67-3.47,6.31-6.07,7.48-11.83,15.01-23.63,22.7-35.31,5.63-8.55,14.49-8.53,20.22,.02,13.17,19.65,26.39,39.28,39.26,59.13,10.45,16.11,1.3,36.16-17.56,36.95-26.62,1.12-53.39,1.86-79.93-.21-33.5-2.61-51.12-39.02-33.62-67.84,11.33-18.65,23.3-36.91,34.95-55.36,15.57-24.65,31.27-49.22,46.59-74.02C96.86,7.09,108.51-.14,124.7,.22c23.32,.52,46.63-.65,69.96-.04,15.23,.4,26.98,6.65,35.11,19.39,10.49,16.43,20.82,32.96,31.03,49.57,9.73,15.84,6.68,38.78-6.66,51.61-5.52,5.31-11.03,10.65-16.74,15.76-2.59,2.32-2.48,4.08-.37,6.67,9.67,11.89,19.18,23.91,28.73,35.89,6.78,8.51,6.95,17.89,2.65,27.26-4.26,9.26-12.57,12.37-22.25,12.27-5.97-.06-9.66-3.4-9.79-8.47-.13-4.99,3.52-8.49,9.46-8.73,3.4-.14,6.22-.8,7.7-4.3,1.46-3.43,.15-6.13-1.94-8.72-11-13.61-21.96-27.24-32.98-40.83-4.9-6.03-4.72-11.85,.81-17.23,7.28-7.09,14.63-14.12,21.95-21.16,8.69-8.36,10.83-21.79,4.49-32.04-10.34-16.72-20.94-33.27-31.66-49.75-4.63-7.11-11.84-10.07-20.08-10.32-7.99-.24-15.99-.06-24.49-.07Z" />
      <path d="M177.68,119.99c-2.49,0-4.49-.04-6.49,.03-7.49,.24-10.24,5.14-6.14,11.32,10.5,15.82,21.15,31.55,31.74,47.31,1.11,1.66,2.3,3.28,3.27,5.02,1.82,3.27,4.33,4.91,8.21,5.55,8.63,1.44,13.69,8.9,12.57,17.53-.99,7.57-7.83,12.75-16.25,12.29-8.3-.45-14.44-6.94-14.27-15.52,.06-3.19-.79-5.78-2.54-8.36-12.11-17.75-24.09-35.59-36.2-53.34-8.89-13.02-6.31-28.45,6.11-35.61,3.24-1.87,6.69-2.91,10.46-2.88,6.5,.04,13-.04,19.49,.02,6.75,.06,11.44-3.3,14.46-8.98,2.61-4.92,.77-9.28-1.57-14.09-4.47-9.22-11-12.31-21.17-11.64-13.6,.91-27.31,.36-40.97,.15-5.03-.08-8.46,1.55-11.11,5.95-5.5,9.13-11.39,18.02-17.1,27.02-3.11,4.9-7.99,6.53-12.24,4.06-4.33-2.52-5.31-7.52-2.16-12.55,6.71-10.73,13.44-21.46,20.38-32.04,4.36-6.65,11.06-9.59,18.77-9.68,17.99-.2,35.99-.27,53.98,0,8.66,.13,15.72,3.89,20.38,11.53,2.17,3.56,4.43,7.05,6.61,10.6,6.03,9.88,5.92,20.03,.39,29.89-5.64,10.05-14.03,16.35-26.13,16.44-4,.03-8-.01-12.48-.02Z" />
    </svg>
  );
}

// New icons for the routing page redesign.
export function IconChevronDown(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M6 9l6 6 6-6" />
    </svg>
  );
}

export function IconChevronUp(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M6 15l6-6 6 6" />
    </svg>
  );
}

export function IconChevronRight(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M9 6l6 6-6 6" />
    </svg>
  );
}

export function IconGripVertical(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <circle cx="9" cy="5" r="1" fill="currentColor" />
      <circle cx="9" cy="12" r="1" fill="currentColor" />
      <circle cx="9" cy="19" r="1" fill="currentColor" />
      <circle cx="15" cy="5" r="1" fill="currentColor" />
      <circle cx="15" cy="12" r="1" fill="currentColor" />
      <circle cx="15" cy="19" r="1" fill="currentColor" />
    </svg>
  );
}

export function IconWand(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M15 4l5 5" />
      <path d="M4 20l11-11" />
      <path d="M14.5 4.5l1 1M19.5 9.5l1 1" />
      <path d="M5 17l2 2" />
    </svg>
  );
}

export function IconBeaker(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M9 3v6L4 19a2 2 0 0 0 1.7 3h12.6A2 2 0 0 0 20 19l-5-10V3" />
      <path d="M8 3h8" />
      <path d="M7 14h10" />
    </svg>
  );
}

export function IconLayers(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M12 2l10 6-10 6L2 8z" />
      <path d="M2 14l10 6 10-6" />
    </svg>
  );
}

export function IconCode(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M8 18l-6-6 6-6" />
      <path d="M16 6l6 6-6 6" />
    </svg>
  );
}

export function IconCheck(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M5 12l5 5 9-12" />
    </svg>
  );
}

export function IconAlertTriangle(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M10.3 3.5L2.4 17a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.5a2 2 0 0 0-3.4 0z" />
      <path d="M12 9v4" />
      <circle cx="12" cy="17" r="0.5" fill="currentColor" />
    </svg>
  );
}

export function IconInfo(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 8h.01" />
      <path d="M11 12h1v5h1" />
    </svg>
  );
}

export function IconArrowRight(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M5 12h14" />
      <path d="M13 5l7 7-7 7" />
    </svg>
  );
}

export function IconZap(p: SVGProps<SVGSVGElement>) {
  return (
    <svg {...base} {...p}>
      <path d="M13 2L3 14h7l-1 8 10-12h-7l1-8z" />
    </svg>
  );
}
