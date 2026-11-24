import {
  argbFromHex,
  Hct,
  hexFromArgb,
  MaterialDynamicColors,
  SchemeVibrant,
} from '@material/material-color-utilities';

export type ThemeSeedId = 'system' | 'indigo' | 'coral' | 'mint' | 'orchid' | 'amber';
export type ThemeMode = 'dark' | 'light' | 'system';

export interface ThemeSeed {
  id: Exclude<ThemeSeedId, 'system'>;
  label: string;
  hex: string;
}

export const THEME_SEEDS: ThemeSeed[] = [
  { id: 'indigo', label: 'Aether Indigo', hex: '#4F65D8' },
  { id: 'coral', label: 'Expressive Coral', hex: '#E05A47' },
  { id: 'mint', label: 'Forest Mint', hex: '#1E9E6E' },
  { id: 'orchid', label: 'Electric Orchid', hex: '#A74AC7' },
  { id: 'amber', label: 'Solar Amber', hex: '#D68910' },
];

export function resolveThemeDark(
  mode: ThemeMode,
  systemScheme?: 'dark' | 'light' | null
): boolean {
  if (mode === 'dark') return true;
  if (mode === 'light') return false;
  if (systemScheme === 'dark') return true;
  if (systemScheme === 'light') return false;
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false;
}

/**
 * Computes and applies all official Material 3 Expressive (`specVersion: '2025'`)
 * CSS custom properties (`--md-sys-color-*`) using Google's official
 * `@material/material-color-utilities` HCT Dynamic Color engine.
 */
export function applyM3ExpressiveTheme(
  seedId: ThemeSeedId,
  mode: ThemeMode,
  systemAccentHex?: string | null,
  systemScheme?: 'dark' | 'light' | null
): void {
  const isDark = resolveThemeDark(mode, systemScheme);

  let sourceHex = '#4F65D8';
  if (seedId === 'system' && systemAccentHex && /^#[0-9a-f]{6}$/i.test(systemAccentHex)) {
    sourceHex = systemAccentHex;
  } else if (seedId !== 'system') {
    sourceHex = THEME_SEEDS.find((s) => s.id === seedId)?.hex ?? sourceHex;
  }

  // Official Google Material 3 Expressive 2025 Dynamic Color Scheme (HCT)
  const hct = Hct.fromInt(argbFromHex(sourceHex));
  const scheme = new SchemeVibrant(hct, isDark, 0.0, '2025');
  const mdc = new MaterialDynamicColors();

  const roles: Array<[string, () => ReturnType<MaterialDynamicColors['primary']>]> = [
    ['--md-sys-color-primary', () => mdc.primary()],
    ['--md-sys-color-on-primary', () => mdc.onPrimary()],
    ['--md-sys-color-primary-container', () => mdc.primaryContainer()],
    ['--md-sys-color-on-primary-container', () => mdc.onPrimaryContainer()],
    ['--md-sys-color-secondary', () => mdc.secondary()],
    ['--md-sys-color-on-secondary', () => mdc.onSecondary()],
    ['--md-sys-color-secondary-container', () => mdc.secondaryContainer()],
    ['--md-sys-color-on-secondary-container', () => mdc.onSecondaryContainer()],
    ['--md-sys-color-tertiary', () => mdc.tertiary()],
    ['--md-sys-color-on-tertiary', () => mdc.onTertiary()],
    ['--md-sys-color-tertiary-container', () => mdc.tertiaryContainer()],
    ['--md-sys-color-on-tertiary-container', () => mdc.onTertiaryContainer()],
    ['--md-sys-color-error', () => mdc.error()],
    ['--md-sys-color-on-error', () => mdc.onError()],
    ['--md-sys-color-error-container', () => mdc.errorContainer()],
    ['--md-sys-color-on-error-container', () => mdc.onErrorContainer()],
    ['--md-sys-color-background', () => mdc.background()],
    ['--md-sys-color-on-background', () => mdc.onBackground()],
    ['--md-sys-color-surface', () => mdc.surface()],
    ['--md-sys-color-surface-dim', () => mdc.surfaceDim()],
    ['--md-sys-color-surface-bright', () => mdc.surfaceBright()],
    ['--md-sys-color-surface-container-lowest', () => mdc.surfaceContainerLowest()],
    ['--md-sys-color-surface-container-low', () => mdc.surfaceContainerLow()],
    ['--md-sys-color-surface-container', () => mdc.surfaceContainer()],
    ['--md-sys-color-surface-container-high', () => mdc.surfaceContainerHigh()],
    ['--md-sys-color-surface-container-highest', () => mdc.surfaceContainerHighest()],
    ['--md-sys-color-on-surface', () => mdc.onSurface()],
    ['--md-sys-color-surface-variant', () => mdc.surfaceVariant()],
    ['--md-sys-color-on-surface-variant', () => mdc.onSurfaceVariant()],
    ['--md-sys-color-outline', () => mdc.outline()],
    ['--md-sys-color-outline-variant', () => mdc.outlineVariant()],
    ['--md-sys-color-inverse-surface', () => mdc.inverseSurface()],
    ['--md-sys-color-inverse-on-surface', () => mdc.inverseOnSurface()],
    ['--md-sys-color-inverse-primary', () => mdc.inversePrimary()],
    ['--md-sys-color-shadow', () => mdc.shadow()],
    ['--md-sys-color-scrim', () => mdc.scrim()],
    ['--md-sys-color-surface-tint', () => mdc.surfaceTint()],
  ];

  const root = document.documentElement;
  root.dataset.theme = isDark ? 'dark' : 'light';
  root.dataset.seed = seedId;

  for (const [cssVar, getter] of roles) {
    root.style.setProperty(cssVar, hexFromArgb(getter().getArgb(scheme)));
  }
}
