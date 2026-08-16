export const colors = {
  background: "#080808",
  sidebar: "#050505",
  surface: "#101010",
  surfaceRaised: "#151515",
  surfaceHover: "#1A1A1A",
  border: "rgba(255,255,255,0.09)",
  borderStrong: "rgba(255,255,255,0.16)",
  textPrimary: "#F5F5F2",
  textSecondary: "#A5A5A0",
  textMuted: "#6E6E69",
  accent: "#FFFFFF",
  warning: "#F3A23A",
  destructive: "#DF6666"
} as const;

export const typeScale = {
  xs: "11px",
  sm: "12px",
  base: "14px",
  md: "16px",
  lg: "20px",
  xl: "26px"
} as const;

export const fontWeights = {
  regular: 400,
  medium: 600,
  bold: 750
} as const;

export const radii = {
  sm: "6px",
  md: "8px",
  lg: "12px",
  pill: "999px"
} as const;

export const layout = {
  sidebarWidth: "244px",
  headerHeight: "72px",
  mobileNavigationHeight: "74px",
  desktopContentMaxWidth: "1440px",
  cardPadding: "18px",
  sectionGap: "16px",
  controlHeight: "40px",
  iconSize: "20px"
} as const;

export const breakpoints = {
  mobile: 0,
  largeMobile: 600,
  tablet: 768,
  desktop: 1024,
  largeDesktop: 1440
} as const;
