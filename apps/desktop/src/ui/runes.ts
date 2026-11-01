/**
 * Rune Icons (https://www.runeicons.com), outline set, 24px grid.
 * Apache License 2.0, Copyright (c) 2026 Rune Icons Team.
 * Only the icons Skiff uses. Strokes use currentColor.
 */

const RUNES = {
  "tools-pencil":
    '<g fill="none"><path d="M15 5L19 9M21.1739 6.81189C21.7026 6.28332 21.9997 5.56636 21.9998 4.81875C21.9999 4.07113 21.703 3.3541 21.1744 2.82539C20.6459 2.29668 19.9289 1.99961 19.1813 1.99951C18.4337 1.99942 17.7166 2.29632 17.1879 2.82489L3.84193 16.1739C3.60975 16.4054 3.43805 16.6904 3.34193 17.0039L2.02093 21.3559C1.99509 21.4424 1.99314 21.5342 2.01529 21.6217C2.03743 21.7092 2.08285 21.7891 2.14673 21.8529C2.21061 21.9167 2.29055 21.962 2.37809 21.984C2.46563 22.006 2.55749 22.0039 2.64393 21.9779L6.99693 20.6579C7.3101 20.5626 7.59511 20.392 7.82693 20.1609L21.1739 6.81189Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "code-terminal":
    '<g fill="none"><path d="M12 19H20M4 17L10 11L4 5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "code-copy":
    '<g fill="none"><path d="M4 16C2.9 16 2 15.1 2 14V4C2 2.9 2.9 2 4 2H14C15.1 2 16 2.9 16 4M10 8H20C21.1046 8 22 8.89543 22 10V20C22 21.1046 21.1046 22 20 22H10C8.89543 22 8 21.1046 8 20V10C8 8.89543 8.89543 8 10 8Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "code-group":
    '<g fill="none"><path d="M3 7V5C3 3.9 3.9 3 5 3H7M17 3H19C20.1 3 21 3.9 21 5V7M21 17V19C21 20.1 20.1 21 19 21H17M7 21H5C3.9 21 3 20.1 3 19V17M8 7H13C13.5523 7 14 7.44772 14 8V11C14 11.5523 13.5523 12 13 12H8C7.44772 12 7 11.5523 7 11V8C7 7.44772 7.44772 7 8 7ZM11 12H16C16.5523 12 17 12.4477 17 13V16C17 16.5523 16.5523 17 16 17H11C10.4477 17 10 16.5523 10 16V13C10 12.4477 10.4477 12 11 12Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "code-ungroup":
    '<g fill="none"><path d="M12 4H6C5.44772 4 5 4.44772 5 5V9C5 9.55228 5.44772 10 6 10H12C12.5523 10 13 9.55228 13 9V5C13 4.44772 12.5523 4 12 4Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M18 14H12C11.4477 14 11 14.4477 11 15V19C11 19.5523 11.4477 20 12 20H18C18.5523 20 19 19.5523 19 19V15C19 14.4477 18.5523 14 18 14Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "code-git-branch":
    '<g fill="none"><path d="M15 6C12.6131 6 10.3239 6.94821 8.63604 8.63604C6.94821 10.3239 6 12.6131 6 15M15 6C15 7.65685 16.3431 9 18 9C19.6569 9 21 7.65685 21 6C21 4.34315 19.6569 3 18 3C16.3431 3 15 4.34315 15 6ZM6 15V3M6 15C4.34315 15 3 16.3431 3 18C3 19.6569 4.34315 21 6 21C7.65685 21 9 19.6569 9 18C9 16.3431 7.65685 15 6 15Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "indicators-plus":
    '<g fill="none"><path d="M5 12H19M12 5V19" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "indicators-minus":
    '<g fill="none"><path d="M5 12H19" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "indicators-x":
    '<g fill="none"><path d="M18 6L6 18M6 6L18 18" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "indicators-check":
    '<g fill="none"><path d="M20 6L9 17L4 12" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "indicators-square-stop":
    '<g fill="none"><path d="M19 3H5C3.89543 3 3 3.89543 3 5V19C3 20.1046 3.89543 21 5 21H19C20.1046 21 21 20.1046 21 19V5C21 3.89543 20.1046 3 19 3Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M14 9H10C9.44772 9 9 9.44772 9 10V14C9 14.5523 9.44772 15 10 15H14C14.5523 15 15 14.5523 15 14V10C15 9.44772 14.5523 9 14 9Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "indicators-square-arrow-out-up-right":
    '<g fill="none"><path d="M21 13V19C21 19.5304 20.7893 20.0391 20.4142 20.4142C20.0391 20.7893 19.5304 21 19 21H5C4.46957 21 3.96086 20.7893 3.58579 20.4142C3.21071 20.0391 3 19.5304 3 19V5C3 4.46957 3.21071 3.96086 3.58579 3.58579C3.96086 3.21071 4.46957 3 5 3H11M21 3L12 12M21 9V3H15" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "tools-trash-2":
    '<g fill="none"><path d="M10 11V17M14 11V17M19 6V20C19 20.5304 18.7893 21.0391 18.4142 21.4142C18.0391 21.7893 17.5304 22 17 22H7C6.46957 22 5.96086 21.7893 5.58579 21.4142C5.21071 21.0391 5 20.5304 5 20V6M3 6H21M8 6V4C8 3.46957 8.21071 2.96086 8.58579 2.58579C8.96086 2.21071 9.46957 2 10 2H14C14.5304 2 15.0391 2.21071 15.4142 2.58579C15.7893 2.96086 16 3.46957 16 4V6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "playback-image":
    '<g fill="none"><path d="M21 14.9999L17.914 11.9139C17.5389 11.539 17.0303 11.3284 16.5 11.3284C15.9697 11.3284 15.4611 11.539 15.086 11.9139L6 20.9999M5 3H19C20.1046 3 21 3.89543 21 5V19C21 20.1046 20.1046 21 19 21H5C3.89543 21 3 20.1046 3 19V5C3 3.89543 3.89543 3 5 3ZM11 9C11 10.1046 10.1046 11 9 11C7.89543 11 7 10.1046 7 9C7 7.89543 7.89543 7 9 7C10.1046 7 11 7.89543 11 9Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "playback-image-plus":
    '<g fill="none"><path d="M16 5H22M19 2V8M21 11.5V19C21 19.5304 20.7893 20.0391 20.4142 20.4142C20.0391 20.7893 19.5304 21 19 21H5C4.46957 21 3.96086 20.7893 3.58579 20.4142C3.21071 20.0391 3 19.5304 3 19V5C3 4.46957 3.21071 3.96086 3.58579 3.58579C3.96086 3.21071 4.46957 3 5 3H12.5M21 14.9999L17.914 11.9139C17.5389 11.539 17.0303 11.3284 16.5 11.3284C15.9697 11.3284 15.4611 11.539 15.086 11.9139L6 20.9999M11 9C11 10.1046 10.1046 11 9 11C7.89543 11 7 10.1046 7 9C7 7.89543 7.89543 7 9 7C10.1046 7 11 7.89543 11 9Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "documents-file-image":
    '<g fill="none"><path d="M14 2H6C5.46957 2 4.96086 2.21072 4.58579 2.58579C4.21071 2.96086 4 3.46957 4 4V20C4 20.5304 4.21071 21.0391 4.58579 21.4142C4.96086 21.7893 5.46957 22 6 22H18C18.5304 22 19.0391 21.7893 19.4142 21.4142C19.7893 21.0391 20 20.5304 20 20V8M14 2C14.3166 1.99949 14.6301 2.06161 14.9225 2.18277C15.215 2.30394 15.4806 2.48176 15.704 2.706L19.292 6.294C19.5168 6.51751 19.6952 6.78335 19.8167 7.07616C19.9382 7.36898 20.0005 7.68297 20 8M14 2V7C14 7.26522 14.1054 7.51957 14.2929 7.70711C14.4804 7.89464 14.7348 8 15 8L20 8M20 17L18.704 15.704C18.252 15.2522 17.6391 14.9983 17 14.9983C16.3609 14.9983 15.748 15.2522 15.296 15.704L9 22M12 12C12 13.1046 11.1046 14 10 14C8.89543 14 8 13.1046 8 12C8 10.8954 8.89543 10 10 10C11.1046 10 12 10.8954 12 12Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "documents-folder-open":
    '<g fill="none"><path d="M6 14L7.5 11.1C7.66307 10.7761 7.91112 10.5027 8.21761 10.3089C8.5241 10.1152 8.8775 10.0084 9.24 9.99997H20M20 9.99997C20.3055 9.99944 20.6071 10.0689 20.8816 10.2031C21.1561 10.3372 21.3963 10.5325 21.5836 10.7738C21.7709 11.0152 21.9004 11.2963 21.9622 11.5955C22.024 11.8947 22.0164 12.2041 21.94 12.5L20.4 18.5C20.2886 18.9315 20.0362 19.3135 19.6829 19.5853C19.3296 19.857 18.8957 20.003 18.45 20H4C3.46957 20 2.96086 19.7893 2.58579 19.4142C2.21071 19.0391 2 18.5304 2 18V4.99997C2 4.46954 2.21071 3.96083 2.58579 3.58576C2.96086 3.21069 3.46957 2.99997 4 2.99997H7.9C8.23449 2.99669 8.56445 3.07736 8.8597 3.23459C9.15495 3.39183 9.40604 3.6206 9.59 3.89997L10.4 5.09997C10.5821 5.3765 10.83 5.60349 11.1215 5.76058C11.413 5.91766 11.7389 5.99992 12.07 5.99997H18C18.5304 5.99997 19.0391 6.21069 19.4142 6.58576C19.7893 6.96083 20 7.46954 20 7.99997V9.99997Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "schedule-refresh-cw":
    '<g fill="none"><path d="M3 12C3 9.61305 3.94821 7.32387 5.63604 5.63604C7.32387 3.94821 9.61305 3 12 3C14.516 3.00947 16.931 3.99122 18.74 5.74L21 8M16 8H21V3M21 12C21 14.3869 20.0518 16.6761 18.364 18.364C16.6761 20.0518 14.3869 21 12 21C9.48395 20.9905 7.06897 20.0088 5.26 18.26L3 16M3 21V16H8" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "layouts-panel-left":
    '<g fill="none"><path d="M9 3V21M5 3H19C20.1046 3 21 3.89543 21 5V19C21 20.1046 20.1046 21 19 21H5C3.89543 21 3 20.1046 3 19V5C3 3.89543 3.89543 3 5 3Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "layouts-panel-bottom":
    '<g fill="none"><path d="M3 15H21M5 3H19C20.1046 3 21 3.89543 21 5V19C21 20.1046 20.1046 21 19 21H5C3.89543 21 3 20.1046 3 19V5C3 3.89543 3.89543 3 5 3Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "tools-sparkles":
    '<g fill="none"><g clip-path="url(#tools-sparkles--clip0_1_2467)"><path d="M20 2V6M22 4H18M11.017 2.81395C11.0598 2.58456 11.1815 2.37737 11.3611 2.22827C11.5406 2.07917 11.7666 1.99756 12 1.99756C12.2333 1.99756 12.4593 2.07917 12.6389 2.22827C12.8184 2.37737 12.9401 2.58456 12.983 2.81395L14.034 8.37195C14.1086 8.7671 14.3006 9.13057 14.585 9.41492C14.8693 9.69928 15.2328 9.89131 15.628 9.96595L21.186 11.017C21.4153 11.0598 21.6225 11.1815 21.7716 11.3611C21.9207 11.5406 22.0023 11.7666 22.0023 12C22.0023 12.2333 21.9207 12.4593 21.7716 12.6389C21.6225 12.8184 21.4153 12.9401 21.186 12.983L15.628 14.034C15.2328 14.1086 14.8693 14.3006 14.585 14.585C14.3006 14.8693 14.1086 15.2328 14.034 15.628L12.983 21.186C12.9401 21.4153 12.8184 21.6225 12.6389 21.7716C12.4593 21.9207 12.2333 22.0023 12 22.0023C11.7666 22.0023 11.5406 21.9207 11.3611 21.7716C11.1815 21.6225 11.0598 21.4153 11.017 21.186L9.96595 15.628C9.89131 15.2328 9.69928 14.8693 9.41492 14.585C9.13057 14.3006 8.7671 14.1086 8.37195 14.034L2.81395 12.983C2.58456 12.9401 2.37737 12.8184 2.22827 12.6389C2.07917 12.4593 1.99756 12.2333 1.99756 12C1.99756 11.7666 2.07917 11.5406 2.22827 11.3611C2.37737 11.1815 2.58456 11.0598 2.81395 11.017L8.37195 9.96595C8.7671 9.89131 9.13057 9.69928 9.41492 9.41492C9.69928 9.13057 9.89131 8.7671 9.96595 8.37195L11.017 2.81395ZM6 20C6 21.1046 5.10457 22 4 22C2.89543 22 2 21.1046 2 20C2 18.8954 2.89543 18 4 18C5.10457 18 6 18.8954 6 20Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g><defs><clipPath id="tools-sparkles--clip0_1_2467"><rect width="24" height="24" fill="white"/></clipPath></defs></g>',
  // Not Rune icons: the pane header's view controls, drawn to the same grid and stroke.
  "view-maximize":
    '<g fill="none"><path d="M8 3H5C3.9 3 3 3.9 3 5V8M16 3H19C20.1 3 21 3.9 21 5V8M21 16V19C21 20.1 20.1 21 19 21H16M8 21H5C3.9 21 3 20.1 3 19V16" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "view-restore":
    '<g fill="none"><path d="M3 8H6C7.1 8 8 7.1 8 6V3M16 3V6C16 7.1 16.9 8 18 8H21M21 16H18C16.9 16 16 16.9 16 18V21M8 21V18C8 16.9 7.1 16 6 16H3" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "view-focus":
    '<g fill="none"><path d="M8 3H5C3.9 3 3 3.9 3 5V8M16 3H19C20.1 3 21 3.9 21 5V8M21 16V19C21 20.1 20.1 21 19 21H16M8 21H5C3.9 21 3 20.1 3 19V16M12 15C13.6569 15 15 13.6569 15 12C15 10.3431 13.6569 9 12 9C10.3431 9 9 10.3431 9 12C9 13.6569 10.3431 15 12 15Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  // Not a Rune icon: a RAM stick drawn to the same grid and stroke.
  "devices-memory":
    '<g fill="none"><path d="M4 6H20C21.1 6 22 6.9 22 8V16H2V8C2 6.9 2.9 6 4 6ZM6 16V19M10 16V19M14 16V19M18 16V19M6 10V12M10 10V12M14 10V12M18 10V12" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "senses-eye":
    '<g fill="none"><path d="M2.06202 12.3481C1.97868 12.1236 1.97868 11.8766 2.06202 11.6521C2.87372 9.68397 4.25153 8.00116 6.02079 6.81701C7.79004 5.63287 9.87106 5.00073 12 5.00073C14.129 5.00073 16.21 5.63287 17.9792 6.81701C19.7485 8.00116 21.1263 9.68397 21.938 11.6521C22.0214 11.8766 22.0214 12.1236 21.938 12.3481C21.1263 14.3163 19.7485 15.9991 17.9792 17.1832C16.21 18.3674 14.129 18.9995 12 18.9995C9.87106 18.9995 7.79004 18.3674 6.02079 17.1832C4.25153 15.9991 2.87372 14.3163 2.06202 12.3481Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/><path d="M12 15C13.6569 15 15 13.6569 15 12C15 10.3431 13.6569 9 12 9C10.3431 9 9 10.3431 9 12C9 13.6569 10.3431 15 12 15Z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
  "senses-eye-off":
    '<g fill="none"><path d="M10.733 5.07599C13.0624 4.7984 15.4186 5.29081 17.4419 6.47804C19.4652 7.66527 21.0442 9.48207 21.938 11.651C22.0214 11.8755 22.0214 12.1225 21.938 12.347C21.5705 13.238 21.0848 14.0755 20.494 14.837M14.084 14.158C13.5182 14.7045 12.7604 15.0069 11.9738 15C11.1872 14.9932 10.4348 14.6777 9.87856 14.1215C9.32233 13.5652 9.00683 12.8128 8.99999 12.0262C8.99316 11.2396 9.29554 10.4818 9.84201 9.91602M17.479 17.499C16.1525 18.2848 14.6725 18.776 13.1394 18.9394C11.6063 19.1028 10.056 18.9345 8.59365 18.4459C7.13133 17.9573 5.79121 17.1599 4.66423 16.1078C3.53725 15.0556 2.64977 13.7734 2.06202 12.348C1.97868 12.1235 1.97868 11.8765 2.06202 11.652C2.94865 9.50189 4.50869 7.69728 6.50802 6.50903M2 2L22 22" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></g>',
} as const satisfies Record<string, string>;
export type RuneName = keyof typeof RUNES;

export const isRune = (name: string): name is RuneName => name in RUNES;

/** An inline icon, `size` px square. `flip` mirrors it left to right. */
export function rune(name: RuneName, size = 14, flip = false): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "rune");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("aria-hidden", "true");
  if (flip) svg.style.transform = "scaleX(-1)";
  svg.innerHTML = RUNES[name];
  return svg;
}
