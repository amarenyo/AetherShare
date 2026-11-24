import React, { useEffect, useState } from 'react';

export type ExpressiveShapeKind =
  | 'softBurst'
  | 'cookie7'
  | 'clover4'
  | 'flower'
  | 'squircle'
  | 'gem';

// Mathematically smooth M3 Expressive rounded polygon paths on a 100x100 viewBox
function makeScallopPath(lobes: number, rOuter: number, rInner: number): string {
  const cx = 50;
  const cy = 50;
  const steps = lobes * 12;
  const pts: string[] = [];
  for (let i = 0; i <= steps; i++) {
    const t = (i / steps) * Math.PI * 2;
    const r = (rOuter + rInner) / 2 + ((rOuter - rInner) / 2) * Math.cos(lobes * t);
    const x = cx + r * Math.cos(t);
    const y = cy + r * Math.sin(t);
    pts.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
  }
  return `${pts.join(' ')} Z`;
}

function makeSuperellipsePath(exp: number, r: number): string {
  const cx = 50;
  const cy = 50;
  const steps = 64;
  const pts: string[] = [];
  for (let i = 0; i <= steps; i++) {
    const t = (i / steps) * Math.PI * 2;
    const ct = Math.cos(t);
    const st = Math.sin(t);
    const x = cx + r * Math.sign(ct) * Math.pow(Math.abs(ct), 2 / exp);
    const y = cy + r * Math.sign(st) * Math.pow(Math.abs(st), 2 / exp);
    pts.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
  }
  return `${pts.join(' ')} Z`;
}

export const SHAPE_PATHS: Record<ExpressiveShapeKind, string> = {
  softBurst: makeScallopPath(10, 48, 39),
  cookie7: makeScallopPath(7, 47, 40),
  clover4: makeScallopPath(4, 48, 33),
  flower: makeScallopPath(6, 48, 36),
  squircle: makeSuperellipsePath(3.6, 45),
  gem: makeScallopPath(5, 47, 39),
};

export const ExpressiveShape: React.FC<{
  kind: ExpressiveShapeKind;
  size?: number;
  className?: string;
  style?: React.CSSProperties;
}> = ({ kind, size = 48, className, style }) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 100 100"
    fill="currentColor"
    className={className}
    style={style}
    aria-hidden="true"
  >
    <path d={SHAPE_PATHS[kind]} />
  </svg>
);

const MORPH_SEQUENCE: ExpressiveShapeKind[] = [
  'softBurst',
  'cookie7',
  'clover4',
  'flower',
  'squircle',
];

/**
 * Material 3 Expressive ContainedLoadingIndicator:
 * Cycles smoothly through expressive rounded polygons with spring rotation.
 */
export const ContainedLoadingIndicator: React.FC<{ size?: number; bare?: boolean }> = ({
  size = 46,
  bare = false,
}) => {
  const [idx, setIdx] = useState(0);

  useEffect(() => {
    const timer = window.setInterval(() => {
      setIdx((n) => (n + 1) % MORPH_SEQUENCE.length);
    }, 700);
    return () => window.clearInterval(timer);
  }, []);

  if (bare) {
    return (
      <ExpressiveShape
        kind={MORPH_SEQUENCE[idx]}
        size={Math.round(size * 0.56)}
        className="m3-loading-spin"
      />
    );
  }

  return (
    <div
      className="m3-loading-contained"
      style={{ width: size, height: size }}
      aria-label="Loading"
    >
      <ExpressiveShape
        kind={MORPH_SEQUENCE[idx]}
        size={Math.round(size * 0.58)}
        className="m3-loading-spin"
      />
    </div>
  );
};

/**
 * Material 3 Expressive LinearWavyProgressIndicator:
 * Sinusoidal animated SVG wave for active transfers, flat inactive track,
 * and trailing stop-indicator dot per m3.material.io guidelines.
 */
export const WavyProgressIndicator: React.FC<{
  progress: number; // 0..100, or < 0 for indeterminate
  active?: boolean;
}> = ({ progress, active = true }) => {
  const clamped = Math.max(0, Math.min(100, progress));
  const indeterminate = progress < 0;
  const activePct = indeterminate ? 48 : clamped;

  // Build repeating sine wave path of wavelength 24px (covers wide containers smoothly)
  const waveLen = 24;
  const amp = active ? 3.2 : 0;
  const midY = 8;
  const segments = 64;
  let d = `M 0 ${midY}`;
  for (let i = 0; i < segments; i++) {
    const x0 = i * waveLen;
    d += ` Q ${x0 + waveLen * 0.25} ${midY - amp}, ${x0 + waveLen * 0.5} ${midY}`;
    d += ` T ${x0 + waveLen} ${midY}`;
  }

  const clipId = React.useId();

  return (
    <div className="wavy-progress-wrap">
      <svg className="wavy-svg" viewBox="0 0 400 16" preserveAspectRatio="none">
        <defs>
          <clipPath id={clipId}>
            <rect
              x={indeterminate ? 0 : 0}
              y="0"
              width={`${activePct}%`}
              height="16"
            />
          </clipPath>
        </defs>

        {/* Inactive track */}
        <line
          x1="4"
          y1={midY}
          x2="392"
          y2={midY}
          stroke="var(--md-sys-color-secondary-container)"
          strokeWidth="4"
          strokeLinecap="round"
        />

        {/* Active wavy track */}
        <g clipPath={`url(#${clipId})`}>
          <path
            d={d}
            fill="none"
            stroke="var(--md-sys-color-primary)"
            strokeWidth="4"
            strokeLinecap="round"
            className={active ? 'wavy-path' : undefined}
          />
        </g>

        {/* M3 stop-indicator dot at track end */}
        {activePct < 98 && (
          <circle cx="394" cy={midY} r="2.2" fill="var(--md-sys-color-primary)" />
        )}
      </svg>
    </div>
  );
};
