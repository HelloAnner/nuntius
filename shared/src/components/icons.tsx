/* Hand-drawn icon set: 24x24 viewBox, 1.6 stroke, round caps. */
import type { SVGProps } from "react";

type P = SVGProps<SVGSVGElement> & { size?: number };

function base({ size = 20, ...props }: P, children: React.ReactNode) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      {...props}
    >
      {children}
    </svg>
  );
}

export const IconDevice = (p: P) =>
  base(p, <>
    <rect x="3" y="4.5" width="18" height="12" rx="2.2" />
    <path d="M8.5 20.5h7M12 16.5v4" />
  </>);

export const IconMobile = (p: P) =>
  base(p, <>
    <rect x="7" y="2.5" width="10" height="19" rx="2.5" />
    <path d="M10.5 18.5h3" />
  </>);

export const IconFolder = (p: P) =>
  base(p, <path d="M3.5 7a2 2 0 0 1 2-2h4l2 2.5h7a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2Z" />);

export const IconGit = (p: P) =>
  base(p, <>
    <circle cx="6.5" cy="6" r="2.4" />
    <circle cx="6.5" cy="18" r="2.4" />
    <circle cx="17.5" cy="9" r="2.4" />
    <path d="M6.5 8.4v7.2M17.5 11.4c0 3.2-2.6 4.6-5.5 4.9" />
  </>);

export const IconChat = (p: P) =>
  base(p, <path d="M20.5 12a8.5 8.5 0 0 1-12.4 7.5L3.5 20.5l1.1-4.4A8.5 8.5 0 1 1 20.5 12Z" />);

export const IconClock = (p: P) =>
  base(p, <>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M12 7.5V12l3 2" />
  </>);

export const IconShield = (p: P) =>
  base(p, <>
    <path d="M12 3 5 5.8v5.4c0 4.3 2.9 7.6 7 9.3 4.1-1.7 7-5 7-9.3V5.8Z" />
    <path d="M12 9.5v3.5" />
    <circle cx="12" cy="16" r="0.4" fill="currentColor" />
  </>);

export const IconSettings = (p: P) =>
  base(p, <>
    <circle cx="12" cy="12" r="3" />
    <path d="M19 12a7 7 0 0 0-.14-1.4l2-1.55-2-3.46-2.35.94a7 7 0 0 0-2.42-1.4L13.7 2.6h-3.4l-.39 2.53a7 7 0 0 0-2.42 1.4l-2.35-.94-2 3.46 2 1.55a7 7 0 0 0 0 2.8l-2 1.55 2 3.46 2.35-.94a7 7 0 0 0 2.42 1.4l.39 2.53h3.4l.39-2.53a7 7 0 0 0 2.42-1.4l2.35.94 2-3.46-2-1.55c.09-.46.14-.93.14-1.4Z" />
  </>);

export const IconChevronLeft = (p: P) => base(p, <path d="m14.5 6-6 6 6 6" />);
export const IconChevronRight = (p: P) => base(p, <path d="m9.5 6 6 6-6 6" />);
export const IconChevronDown = (p: P) => base(p, <path d="m6 9.5 6 6 6-6" />);
export const IconArrowUp = (p: P) => base(p, <path d="M12 19V5M5.5 11.5 12 5l6.5 6.5" />);
export const IconArrowDown = (p: P) => base(p, <path d="M12 5v14M5.5 12.5 12 19l6.5-6.5" />);

export const IconPlus = (p: P) => base(p, <path d="M12 5v14M5 12h14" />);
export const IconImage = (p: P) =>
  base(p, <>
    <rect x="3.5" y="4" width="17" height="16" rx="2.3" />
    <circle cx="9" cy="9.5" r="1.7" />
    <path d="m5.5 17 4.2-4.2 3.1 3 2.1-2.1 3.6 3.3" />
  </>);

export const IconStop = (p: P) =>
  base(p, <rect x="7" y="7" width="10" height="10" rx="2" fill="currentColor" stroke="none" />);

export const IconArchive = (p: P) =>
  base(p, <>
    <rect x="3.5" y="4.5" width="17" height="4.5" rx="1.4" />
    <path d="M5 9v9a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V9M10 13.5h4" />
  </>);

export const IconSearch = (p: P) =>
  base(p, <>
    <circle cx="11" cy="11" r="6.5" />
    <path d="m16 16 4.5 4.5" />
  </>);

export const IconCopy = (p: P) =>
  base(p, <>
    <rect x="9" y="9" width="11.5" height="11.5" rx="2" />
    <path d="M5.5 15h-1a2 2 0 0 1-2-2V5.5a2 2 0 0 1 2-2H12a2 2 0 0 1 2 2v1" />
  </>);

export const IconCheck = (p: P) => base(p, <path d="m4.5 12.5 5 5 10-11" />);
export const IconX = (p: P) => base(p, <path d="M6 6l12 12M18 6 6 18" />);

export const IconAlert = (p: P) =>
  base(p, <>
    <path d="M12 3.5 2.8 19.5h18.4Z" />
    <path d="M12 10v4" />
    <circle cx="12" cy="16.8" r="0.4" fill="currentColor" />
  </>);

export const IconRefresh = (p: P) =>
  base(p, <>
    <path d="M20 11a8 8 0 1 0-2.3 6.3" />
    <path d="M20 5v6h-6" />
  </>);

export const IconFile = (p: P) =>
  base(p, <>
    <path d="M6 3.5h7.5L19 9v11a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 5 20V5a1.5 1.5 0 0 1 1-1.5Z" />
    <path d="M13.5 3.5V9H19" />
  </>);

export const IconTerminal = (p: P) =>
  base(p, <>
    <rect x="3" y="4.5" width="18" height="15" rx="2.2" />
    <path d="m7 9.5 3 2.7-3 2.8M12.5 15h4.5" />
  </>);

export const IconThought = (p: P) =>
  base(p, <>
    <path d="M9.5 18.5a4.8 4.8 0 1 1 1.7-9.3 5.4 5.4 0 1 1 6.6 6.6c-.6 1.6-2 2.7-3.7 2.7Z" />
    <path d="M9 21.5h5" />
  </>);

export const IconSparkle = (p: P) =>
  base(p, <path d="M12 3.5c.7 4.6 3.9 7.8 8.5 8.5-4.6.7-7.8 3.9-8.5 8.5-.7-4.6-3.9-7.8-8.5-8.5 4.6-.7 7.8-3.9 8.5-8.5Z" />);

export const IconLogout = (p: P) =>
  base(p, <>
    <path d="M14 4.5H7a2 2 0 0 0-2 2v11a2 2 0 0 0 2 2h7" />
    <path d="M10.5 12h10M17 8.5l3.5 3.5-3.5 3.5" />
  </>);

export const IconLink = (p: P) =>
  base(p, <>
    <path d="M10 14a4.5 4.5 0 0 0 6.4.4l3-3a4.5 4.5 0 0 0-6.4-6.4l-1.5 1.5" />
    <path d="M14 10a4.5 4.5 0 0 0-6.4-.4l-3 3a4.5 4.5 0 0 0 6.4 6.4l1.5-1.5" />
  </>);

export const IconBolt = (p: P) =>
  base(p, <path d="M13 2.5 4.5 13.5H11L10 21.5l8.5-11H12Z" />);

export const IconTool = (p: P) =>
  base(p, <path d="M14.7 6.3a4.6 4.6 0 0 0-6 5.9L3.4 17.5a2.05 2.05 0 1 0 2.9 2.9l5.3-5.3a4.6 4.6 0 0 0 5.9-6l-2.9 2.9-2.3-.7-.7-2.3Z" />);

export const IconHistory = (p: P) =>
  base(p, <>
    <path d="M3.5 12a8.5 8.5 0 1 1 2.5 6" />
    <path d="M3.5 12H7M3.5 12V8.5M12 8v4.2l3 1.8" />
  </>);

export const IconMore = (p: P) =>
  base(p, <>
    <circle cx="5.5" cy="12" r="1" fill="currentColor" />
    <circle cx="12" cy="12" r="1" fill="currentColor" />
    <circle cx="18.5" cy="12" r="1" fill="currentColor" />
  </>);

export const IconSun = (p: P) =>
  base(p, <>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2.5v2.2M12 19.3v2.2M2.5 12h2.2M19.3 12h2.2M5.2 5.2l1.6 1.6M17.2 17.2l1.6 1.6M18.8 5.2l-1.6 1.6M6.8 17.2l-1.6 1.6" />
  </>);

export const IconMoon = (p: P) =>
  base(p, <path d="M20.5 14.5A8.5 8.5 0 0 1 9.5 3.5a8.5 8.5 0 1 0 11 11Z" />);

export const IconKey = (p: P) =>
  base(p, <>
    <circle cx="8" cy="15.5" r="4.5" />
    <path d="m11.2 12.3 8.3-8.3M16.5 7l2.5 2.5M13.8 9.7l2 2" />
  </>);

export const IconSwitch = (p: P) =>
  base(p, <>
    <path d="M4 8h13M13.5 4.5 17 8l-3.5 3.5" />
    <path d="M20 16H7M10.5 12.5 7 16l3.5 3.5" />
  </>);
