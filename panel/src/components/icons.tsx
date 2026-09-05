// Stroke icons on a 24 px grid, drawn in currentColor, from the axioma
// design system.

import type { ReactNode } from 'react'

const paths = {
  chevronDown: <path d="M6 9l6 6 6-6" />,
  reload: (
    <>
      <path d="M21 12a9 9 0 1 1-3-6.7L21 8" />
      <path d="M21 3v5h-5" />
    </>
  ),
  computer: (
    <>
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <path d="M8 21h8M12 17v4" />
    </>
  ),
  tablet: (
    <>
      <rect x="4" y="2" width="16" height="20" rx="2" />
      <path d="M12 18h.01" />
    </>
  ),
  phone: (
    <>
      <rect x="5" y="2" width="14" height="20" rx="2" />
      <path d="M12 18h.01" />
    </>
  ),
  website: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" />
    </>
  ),
  pages: (
    <>
      <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
      <path d="M14 2v4a2 2 0 0 0 2 2h4" />
    </>
  ),
  components: (
    <>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <path d="M3 8h18M3 16h18" />
    </>
  ),
  media: (
    <>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <circle cx="9" cy="9" r="2" />
      <path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21" />
    </>
  ),
  settings: (
    <>
      <path d="M20 7h-9M14 17H5" />
      <circle cx="17" cy="17" r="3" />
      <circle cx="7" cy="7" r="3" />
    </>
  ),
  history: (
    <>
      <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
      <path d="M3 3v5h5" />
      <path d="M12 7v5l3 2" />
    </>
  ),
  newChat: (
    <>
      <path d="M12 20h9" />
      <path d="M16.4 3.6a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z" />
    </>
  ),
  user: (
    <>
      <circle cx="12" cy="8" r="4" />
      <path d="M4 21a8 8 0 0 1 16 0" />
    </>
  ),
  sun: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41" />
    </>
  ),
  moon: <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z" />,
  activity: <path d="M3 12h4l3-8 4 16 3-8h4" />,
  people: (
    <>
      <circle cx="9" cy="8" r="3.5" />
      <path d="M2.5 20a6.5 6.5 0 0 1 13 0" />
      <path d="M16 4.6a3.5 3.5 0 0 1 0 6.8M18 14.4a6.5 6.5 0 0 1 3.5 5.6" />
    </>
  ),
  more: (
    <>
      <circle cx="5" cy="12" r="1" />
      <circle cx="12" cy="12" r="1" />
      <circle cx="19" cy="12" r="1" />
    </>
  ),
  close: <path d="M6 6l12 12M18 6 6 18" />,
  copy: (
    <>
      <rect x="9" y="9" width="12" height="12" rx="2" />
      <path d="M5 15a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2" />
    </>
  ),
  check: <path d="M5 12.5 10 17 19 7" />,
  plus: <path d="M12 5v14M5 12h14" />,
  send: <path d="m5 12 7-7 7 7M12 19V5" />,
  bolt: <path d="M13 2 4 14h7l-1 8 9-12h-7Z" />,
  chat: <path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z" />,
} satisfies Record<string, ReactNode>

export type IconName = keyof typeof paths

interface IconProps {
  name: IconName
  size?: number
  strokeWidth?: number
}

export function Icon({ name, size = 20, strokeWidth = 1.5 }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flex: '0 0 auto' }}
    >
      {paths[name]}
    </svg>
  )
}

const petals = [
  'M 88.75,47.85 C 68.56,47.85 52.14,31.43 52.14,11.24 C 52.14,7.93 52.58,4.65 53.45,1.49 C 53.69,0.61 54.50,0 55.41,0 C 55.49,0 55.56,0.00 55.64,0.01 C 66.90,1.27 77.53,6.39 85.56,14.43 C 93.60,22.46 98.72,33.09 99.98,44.35 C 100.09,45.35 99.47,46.27 98.50,46.54 C 95.34,47.41 92.06,47.85 88.75,47.85',
  'M 11.24,47.85 C 7.93,47.85 4.65,47.41 1.49,46.54 C 0.52,46.27 -0.09,45.35 0.01,44.35 C 1.27,33.09 6.39,22.46 14.43,14.43 C 22.46,6.39 33.09,1.27 44.35,0.01 C 44.43,0.00 44.50,0 44.58,0 C 45.49,0 46.30,0.61 46.54,1.49 C 47.41,4.65 47.85,7.93 47.85,11.24 C 47.85,31.43 31.43,47.85 11.24,47.85',
  'M 44.58,99.99 C 44.50,99.99 44.43,99.99 44.35,99.98 C 33.09,98.72 22.46,93.60 14.43,85.56 C 6.39,77.53 1.27,66.90 0.01,55.64 C -0.09,54.64 0.52,53.72 1.49,53.45 C 4.65,52.58 7.93,52.14 11.24,52.14 C 31.43,52.14 47.85,68.56 47.85,88.75 C 47.85,92.06 47.41,95.34 46.54,98.50 C 46.30,99.38 45.49,99.99 44.58,99.99',
  'M 55.41,99.99 C 54.50,99.99 53.69,99.38 53.45,98.50 C 52.58,95.34 52.14,92.06 52.14,88.75 C 52.14,68.56 68.56,52.14 88.75,52.14 C 92.06,52.14 95.34,52.58 98.50,53.45 C 99.47,53.72 100.09,54.64 99.98,55.64 C 98.72,66.90 93.60,77.53 85.56,85.56 C 77.53,93.60 66.90,98.72 55.64,99.98 C 55.56,99.99 55.49,99.99 55.41,99.99',
]

interface MarkProps {
  size?: number
  label?: string
  className?: string
}

/** The axioma mark: four petals around an empty centre, in fg. */
export function Mark({ size = 24, label, className }: MarkProps) {
  return (
    <svg
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      width={size}
      height={size}
      viewBox="-4 -4 108 108"
      className={className}
      style={{ flex: '0 0 auto' }}
    >
      {petals.map((d) => (
        <path key={d} d={d} fill="currentColor" />
      ))}
    </svg>
  )
}
