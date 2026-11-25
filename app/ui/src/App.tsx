import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  apiBase,
  fetchStatus,
  formatBytes,
  openPathNative,
  pickFilesNative,
  pickFolderNative,
  sendFilesToDevice,
  sendTransferAction,
  setAirDropEnabled,
  setVisibility,
  startDiscovery,
  stopDiscovery,
  updateSettings,
} from './api/client';
import {
  ContainedLoadingIndicator,
  ExpressiveShape,
  type ExpressiveShapeKind,
  WavyProgressIndicator,
} from './components/ExpressiveShapes';
import { Icon, type IconName } from './components/Icon';
import {
  getTranslations,
  type LangPref,
  resolveLang,
  type Translations,
} from './i18n/translations';
import {
  applyM3ExpressiveTheme,
  THEME_SEEDS,
  type ThemeMode,
  type ThemeSeedId,
} from './theme/m3Expressive';
import type {
  ChannelMessage,
  EndpointInfo,
  PickedFile,
  StatusResponse,
  TransferRecord,
  VisibilityMode,
} from './types';

type ViewMode = 'home' | 'transfers' | 'settings';

/** Thin React 19 wrapper for official <md-switch> DOM 'change' event */
const MdSwitch: React.FC<{
  selected: boolean;
  onChange: (next: boolean) => void;
  ariaLabel?: string;
}> = ({ selected, onChange, ariaLabel }) => {
  const ref = useRef<HTMLElement & { selected?: boolean }>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.selected = selected;
  }, [selected]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const handler = () => onChange(Boolean(el.selected));
    el.addEventListener('change', handler);
    return () => el.removeEventListener('change', handler);
  }, [onChange]);

  return <md-switch ref={ref} icons aria-label={ariaLabel} />;
};

/** Thin React 19 wrapper for official <md-input-chip> DOM 'remove' event */
const MdFileChip: React.FC<{
  label: string;
  title?: string;
  isFolder?: boolean;
  onRemove: () => void;
}> = ({ label, title, isFolder, onRemove }) => {
  const ref = useRef<HTMLElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const handler = (ev: Event) => {
      ev.preventDefault();
      onRemove();
    };
    el.addEventListener('remove', handler);
    return () => el.removeEventListener('remove', handler);
  }, [onRemove]);

  return (
    <md-input-chip ref={ref} label={label} title={title} remove-only={false}>
      <span slot="icon" style={{ display: 'inline-flex', alignItems: 'center' }}>
        <Icon name={isFolder ? 'folder_open' : 'description'} size={17} />
      </span>
    </md-input-chip>
  );
};

function deviceVisual(ep: EndpointInfo): {
  shape: ExpressiveShapeKind;
  icon: IconName;
  tintBg: string;
  tintFg: string;
} {
  if (ep.id.startsWith('airdrop://') || ep.protocol === 'airdrop') {
    return {
      shape: 'softBurst',
      icon: 'apple',
      tintBg: 'var(--md-sys-color-tertiary-container)',
      tintFg: 'var(--md-sys-color-on-tertiary-container)',
    };
  }
  const rt = (ep.rtype || '').toLowerCase();
  if (rt.includes('phone')) {
    return {
      shape: 'cookie7',
      icon: 'smartphone',
      tintBg: 'var(--md-sys-color-primary-container)',
      tintFg: 'var(--md-sys-color-on-primary-container)',
    };
  }
  if (rt.includes('tablet')) {
    return {
      shape: 'squircle',
      icon: 'tablet',
      tintBg: 'var(--md-sys-color-secondary-container)',
      tintFg: 'var(--md-sys-color-on-secondary-container)',
    };
  }
  return {
    shape: 'gem',
    icon: 'laptop',
    tintBg: 'var(--md-sys-color-primary-container)',
    tintFg: 'var(--md-sys-color-on-primary-container)',
  };
}

function extractFiles(payload: unknown): string[] {
  if (!payload || typeof payload !== 'object') return [];
  const p = payload as Record<string, unknown>;
  if (Array.isArray(p.Files)) return p.Files.map(String);
  if (typeof p.Text === 'string') return [p.Text];
  return [];
}

function stateLabel(state: string, tr: Translations): string {
  switch (state) {
    case 'WaitingForUserConsent':
      return tr.stateWaiting;
    case 'ReceivingFiles':
      return tr.stateReceiving;
    case 'SendingFiles':
      return tr.stateSending;
    case 'Finished':
      return tr.stateFinished;
    case 'Rejected':
      return tr.stateRejected;
    case 'Cancelled':
      return tr.stateCancelled;
    case 'Disconnected':
      return tr.stateDisconnected;
    default:
      return tr.stateConnecting;
  }
}

function visibilityLabel(v: VisibilityMode, tr: Translations): string {
  switch (v) {
    case 'Visible':
      return tr.visVisible;
    case 'Temporarily':
      return tr.visTemp;
    case 'Invisible':
      return tr.visInvisible;
  }
}

export default function App() {
  const [view, setView] = useState<ViewMode>('home');
  const [seed, setSeed] = useState<ThemeSeedId>(
    () => (localStorage.getItem('aethershare_seed_v2') as ThemeSeedId) || 'system'
  );
  const [themeMode, setThemeMode] = useState<ThemeMode>(
    () => (localStorage.getItem('aethershare_theme_mode_v2') as ThemeMode) || 'system'
  );
  const [langPref, setLangPref] = useState<LangPref>(
    () => (localStorage.getItem('aethershare_lang') as LangPref) || 'system'
  );

  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [showDevInfo, setShowDevInfo] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<{
    state: 'idle' | 'checking' | 'latest' | 'available' | 'error';
    latestVersion?: string;
    releaseUrl?: string;
  }>({ state: 'idle' });
  const [scanning, setScanning] = useState(false);
  const [airdropBusy, setAirdropBusy] = useState(false);
  const [devices, setDevices] = useState<Record<string, EndpointInfo>>({});
  const [queuedFiles, setQueuedFiles] = useState<PickedFile[]>([]);
  const [dragging, setDragging] = useState(false);
  const [transfers, setTransfers] = useState<Record<string, TransferRecord>>({});
  const [snack, setSnack] = useState<string | null>(null);

  const snackTimer = useRef<number | null>(null);
  const burstStopTimer = useRef<number | null>(null);
  const periodicTimer = useRef<number | null>(null);
  const visMenuRef = useRef<HTMLElement & { open?: boolean }>(null);

  const activeLang = useMemo(
    () => resolveLang(langPref, status?.system_lang),
    [langPref, status?.system_lang]
  );
  const tr = useMemo(() => getTranslations(activeLang), [activeLang]);
  const trRef = useRef(tr);
  trRef.current = tr;

  // Sync HTML lang attribute + backend notification language preference
  useEffect(() => {
    document.documentElement.lang = activeLang;
    localStorage.setItem('aethershare_lang', langPref);
    updateSettings({ language: langPref }).catch(() => {});
  }, [langPref, activeLang]);

  const showSnack = useCallback((msg: string) => {
    setSnack(msg);
    if (snackTimer.current) window.clearTimeout(snackTimer.current);
    snackTimer.current = window.setTimeout(() => setSnack(null), 2800);
  }, []);

  // Apply official @material/material-color-utilities 2025 Dynamic Scheme
  useEffect(() => {
    applyM3ExpressiveTheme(
      seed,
      themeMode,
      status?.system_accent,
      status?.system_color_scheme
    );
    localStorage.setItem('aethershare_seed_v2', seed);
    localStorage.setItem('aethershare_theme_mode_v2', themeMode);
  }, [seed, themeMode, status?.system_accent, status?.system_color_scheme]);

  useEffect(() => {
    if (themeMode !== 'system') return;
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const handler = () =>
      applyM3ExpressiveTheme(
        seed,
        'system',
        status?.system_accent,
        status?.system_color_scheme
      );
    mq.addEventListener('change', handler);
    return () => mq.removeEventListener('change', handler);
  }, [seed, themeMode, status?.system_accent, status?.system_color_scheme]);

  const refreshStatus = useCallback(async () => {
    try {
      const s = await fetchStatus();
      setStatus(s);
    } catch {
      // ignore during startup
    }
  }, []);

  const handleCheckUpdates = useCallback(async () => {
    const currentVer = status?.version || '0.1.0';
    setUpdateInfo({ state: 'checking' });
    try {
      const res = await fetch(
        'https://api.github.com/repos/amarenyo/AetherShare/releases/latest',
        { headers: { Accept: 'application/vnd.github+json' } }
      );
      if (res.status === 404) {
        // No release published yet — current build is the latest
        setUpdateInfo({ state: 'latest', latestVersion: currentVer });
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as { tag_name?: string; html_url?: string };
      const latestVer = (data.tag_name || '').replace(/^[vV]/, '').trim();
      const parse = (v: string) =>
        v
          .split('-')[0]
          .split('.')
          .map((n) => parseInt(n, 10) || 0);
      const a = parse(latestVer);
      const b = parse(currentVer);
      const newer =
        a[0] > b[0] ||
        (a[0] === b[0] && a[1] > b[1]) ||
        (a[0] === b[0] && a[1] === b[1] && (a[2] || 0) > (b[2] || 0));
      if (latestVer && newer) {
        setUpdateInfo({
          state: 'available',
          latestVersion: latestVer,
          releaseUrl:
            data.html_url || 'https://github.com/amarenyo/AetherShare/releases/latest',
        });
      } else {
        setUpdateInfo({ state: 'latest', latestVersion: currentVer });
      }
    } catch {
      setUpdateInfo({ state: 'error' });
    }
  }, [status?.version]);

  useEffect(() => {
    refreshStatus();
    const id = window.setInterval(refreshStatus, 10000);
    return () => window.clearInterval(id);
  }, [refreshStatus]);

  // Focus-aware discovery burst + automatic periodic refresh while focused
  const triggerScanBurst = useCallback(async (durationMs = 7000, clearStale = false) => {
    if (burstStopTimer.current) {
      window.clearTimeout(burstStopTimer.current);
      burstStopTimer.current = null;
    }
    if (clearStale) {
      setDevices({});
    }
    setScanning(true);
    try {
      await startDiscovery();
    } catch {
      setScanning(false);
      return;
    }
    burstStopTimer.current = window.setTimeout(async () => {
      try {
        await stopDiscovery();
      } catch {
        // ignore
      }
      setScanning(false);
    }, durationMs);
  }, []);

  const stopScanNow = useCallback(async () => {
    if (burstStopTimer.current) {
      window.clearTimeout(burstStopTimer.current);
      burstStopTimer.current = null;
    }
    setScanning(false);
    try {
      await stopDiscovery();
    } catch {
      // ignore
    }
  }, []);

  useEffect(() => {
    const startFocusedLoop = () => {
      if (periodicTimer.current) window.clearInterval(periodicTimer.current);
      triggerScanBurst(7000, false);
      periodicTimer.current = window.setInterval(() => {
        if (document.visibilityState === 'visible' && document.hasFocus()) {
          triggerScanBurst(5500, false);
        }
      }, 12000);
    };

    const handleBlurOrHide = () => {
      if (periodicTimer.current) {
        window.clearInterval(periodicTimer.current);
        periodicTimer.current = null;
      }
      stopScanNow();
    };

    const onFocus = () => {
      refreshStatus();
      startFocusedLoop();
    };
    const onBlur = () => handleBlurOrHide();
    const onVisChange = () => {
      if (document.visibilityState === 'visible') {
        refreshStatus();
        startFocusedLoop();
      } else {
        handleBlurOrHide();
      }
    };

    if (document.visibilityState === 'visible') {
      startFocusedLoop();
    }

    window.addEventListener('focus', onFocus);
    window.addEventListener('blur', onBlur);
    document.addEventListener('visibilitychange', onVisChange);

    return () => {
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('blur', onBlur);
      document.removeEventListener('visibilitychange', onVisChange);
      handleBlurOrHide();
    };
  }, [triggerScanBurst, stopScanNow, refreshStatus]);

  // Native Tauri OS Drag & Drop listener
  useEffect(() => {
    const onNativeDrop = (ev: Event) => {
      const custom = ev as CustomEvent<{ paths: string[] }>;
      if (!custom.detail?.paths?.length) return;
      const incoming: PickedFile[] = custom.detail.paths.map((p) => ({
        path: p,
        name: p.split('/').pop() || p,
        size: 0,
      }));
      setQueuedFiles((prev) => {
        const seen = new Set(prev.map((f) => f.path));
        return [...prev, ...incoming.filter((f) => !seen.has(f.path))];
      });
      setView('home');
      showSnack(trRef.current.snackItemsAdded(incoming.length));
    };
    window.addEventListener('aethershare:drop-files', onNativeDrop);
    return () => window.removeEventListener('aethershare:drop-files', onNativeDrop);
  }, [showSnack]);

  // SSE stream
  useEffect(() => {
    let es: EventSource | null = null;
    let retryTimer: number | null = null;

    const connect = () => {
      es = new EventSource(`${apiBase()}/api/events`);

      es.addEventListener('endpoint', (ev) => {
        try {
          const ep: EndpointInfo = JSON.parse((ev as MessageEvent).data);
          setDevices((prev) => {
            const next = { ...prev };
            if (ep.present !== true) {
              delete next[ep.id];
            } else {
              next[ep.id] = ep;
            }
            return next;
          });
        } catch {
          // ignore
        }
      });

      es.addEventListener('transfer', (ev) => {
        try {
          const cm: ChannelMessage = JSON.parse((ev as MessageEvent).data);
          const client = cm.msg?.Client;
          if (!client) return;
          setTransfers((prev) => {
            const old = prev[cm.id];
            const meta = client.metadata;
            const files = extractFiles(meta?.payload);
            const rec: TransferRecord = {
              id: cm.id,
              kind: client.kind || old?.kind || 'Inbound',
              peerName: meta?.source?.name || old?.peerName || trRef.current.unknownDevice,
              state: client.state || old?.state || 'Initial',
              pinCode: meta?.pin_code || old?.pinCode,
              files: files.length ? files : old?.files || [],
              savedPaths: meta?.saved_paths?.length ? meta.saved_paths : old?.savedPaths || [],
              destination: meta?.destination || old?.destination,
              totalBytes: meta?.total_bytes ?? old?.totalBytes ?? 0,
              ackBytes: meta?.ack_bytes ?? old?.ackBytes ?? 0,
              updatedAt: Date.now(),
            };
            return { ...prev, [cm.id]: rec };
          });
        } catch {
          // ignore
        }
      });

      es.onerror = () => {
        es?.close();
        retryTimer = window.setTimeout(connect, 2500);
      };
    };

    connect();
    return () => {
      es?.close();
      if (retryTimer) window.clearTimeout(retryTimer);
    };
  }, []);

  const handleVisibility = async (v: VisibilityMode) => {
    if (visMenuRef.current) visMenuRef.current.open = false;
    await setVisibility(v);
    setStatus((s) => (s ? { ...s, visibility: v } : s));
    showSnack(visibilityLabel(v, tr));
  };

  const handlePickFiles = async () => {
    try {
      const picked = await pickFilesNative();
      if (picked.length > 0) {
        setQueuedFiles((prev) => {
          const seen = new Set(prev.map((f) => f.path));
          return [...prev, ...picked.filter((f) => !seen.has(f.path))];
        });
      }
    } catch {
      showSnack(tr.snackPickFilesFailed);
    }
  };

  const handlePickSendFolder = async () => {
    try {
      const folder = await pickFolderNative();
      if (!folder) return;
      const name = folder.split('/').filter(Boolean).pop() || folder;
      setQueuedFiles((prev) => {
        if (prev.some((f) => f.path === folder)) return prev;
        return [...prev, { path: folder, name: `${name}/`, size: 0 }];
      });
    } catch {
      showSnack(tr.snackPickFolderFailed);
    }
  };

  const handleSendTo = async (ep: EndpointInfo) => {
    let filesToSend = queuedFiles.map((f) => f.path);
    if (filesToSend.length === 0) {
      const picked = await pickFilesNative();
      if (picked.length === 0) return;
      setQueuedFiles(picked);
      filesToSend = picked.map((f) => f.path);
    }
    try {
      await sendFilesToDevice(ep, filesToSend);
      showSnack(tr.snackSendingTo(ep.name || tr.deviceFallback));
      setView('transfers');
    } catch {
      showSnack(tr.snackSendFailed);
    }
  };

  const handlePickDownloadDir = async () => {
    const dir = await pickFolderNative();
    if (!dir) return;
    await updateSettings({ download_dir: dir });
    setStatus((s) => (s ? { ...s, download_dir: dir } : s));
    showSnack(tr.snackFolderUpdated);
  };

  const handleToggleAutoAccept = useCallback(
    async (next: boolean) => {
      await updateSettings({ auto_accept: next });
      setStatus((s) => (s ? { ...s, auto_accept: next } : s));
      showSnack(next ? trRef.current.snackAutoAcceptOn : trRef.current.snackAutoAcceptOff);
    },
    [showSnack]
  );

  // Unified Quick Share + AirDrop devices (deduplicated by name)
  const unifiedDevices = useMemo(() => {
    const byKey = new Map<string, EndpointInfo>();
    for (const ep of Object.values(devices)) {
      if (ep.present !== true) continue;
      const label = (ep.name || '').trim();
      if (!label) continue;
      const key = `${ep.id.startsWith('airdrop://') ? 'ad:' : 'qs:'}${label.toLowerCase()}`;
      const existing = byKey.get(key);
      if (!existing || (existing.ble_addr && !ep.ble_addr)) {
        byKey.set(key, ep);
      }
    }
    return Array.from(byKey.values());
  }, [devices]);

  const transferList = useMemo(
    () => Object.values(transfers).sort((a, b) => b.updatedAt - a.updatedAt),
    [transfers]
  );

  const pendingIncoming = useMemo(
    () =>
      transferList.filter(
        (t) => t.kind === 'Inbound' && t.state === 'WaitingForUserConsent'
      ),
    [transferList]
  );

  const activeTransfersCount = useMemo(
    () =>
      transferList.filter(
        (t) =>
          t.state === 'ReceivingFiles' ||
          t.state === 'SendingFiles' ||
          t.state === 'WaitingForUserConsent'
      ).length,
    [transferList]
  );

  const totalQueueSize = useMemo(
    () => queuedFiles.reduce((acc, f) => acc + (f.size || 0), 0),
    [queuedFiles]
  );

  const vis: VisibilityMode = status?.visibility || 'Visible';

  return (
    <div
      className="simple-shell"
      onDragOver={(e) => {
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setDragging(false);
      }}
      onDrop={(e) => {
        e.preventDefault();
        setDragging(false);
        const dtFiles = Array.from(e.dataTransfer.files || []);
        const withPaths = dtFiles
          .map((f) => ({
            path: (f as unknown as { path?: string }).path || '',
            name: f.name,
            size: f.size,
          }))
          .filter((f) => f.path);
        if (withPaths.length > 0) {
          setQueuedFiles((prev) => {
            const seen = new Set(prev.map((x) => x.path));
            return [...prev, ...withPaths.filter((x) => !seen.has(x.path))];
          });
          setView('home');
          showSnack(tr.snackFilesAdded(withPaths.length));
        }
      }}
    >
      {/* ===================== Clean Top Bar (Official @material/web) ===================== */}
      <header className="top-bar">
        <button className="top-brand" onClick={() => setView('home')}>
          <md-ripple />
          <span className="top-brand-emblem">
            <ExpressiveShape kind="softBurst" size={34} />
            <span style={{ color: 'var(--md-sys-color-on-primary)', display: 'grid', placeItems: 'center', zIndex: 2 }}>
              <Icon name="bolt" size={16} />
            </span>
          </span>
          <span className="m3-title-large">{status?.device_name || 'AetherShare'}</span>
        </button>

        <div className="top-actions">
          {/* Official <md-menu> anchored to Visibility Pill */}
          <div className="vis-anchor-wrap">
            <button
              id="vis-menu-anchor"
              className="vis-pill-btn"
              onClick={() => {
                if (visMenuRef.current) {
                  visMenuRef.current.open = !visMenuRef.current.open;
                }
              }}
            >
              <md-ripple />
              <span className={`vis-pill-dot ${vis === 'Invisible' ? 'off' : ''}`}>
                <Icon
                  name={
                    vis === 'Invisible'
                      ? 'visibility_off'
                      : vis === 'Temporarily'
                        ? 'schedule'
                        : 'visibility'
                  }
                  size={16}
                />
              </span>
              <span>{visibilityLabel(vis, tr)}</span>
              <Icon name="expand_more" size={18} />
            </button>

            <md-menu
              ref={visMenuRef}
              anchor="vis-menu-anchor"
              positioning="popover"
              anchor-corner="end-end"
              menu-corner="start-end"
            >
              <md-menu-item
                selected={vis === 'Visible' || undefined}
                onClick={() => handleVisibility('Visible')}
              >
                <span slot="start"><Icon name="visibility" size={20} /></span>
                <div slot="headline">{tr.visVisible}</div>
                <div slot="supporting-text">{tr.visVisibleSub}</div>
              </md-menu-item>
              <md-menu-item
                selected={vis === 'Temporarily' || undefined}
                onClick={() => handleVisibility('Temporarily')}
              >
                <span slot="start"><Icon name="schedule" size={20} /></span>
                <div slot="headline">{tr.visTemp}</div>
                <div slot="supporting-text">{tr.visTempSub}</div>
              </md-menu-item>
              <md-menu-item
                selected={vis === 'Invisible' || undefined}
                onClick={() => handleVisibility('Invisible')}
              >
                <span slot="start"><Icon name="visibility_off" size={20} /></span>
                <div slot="headline">{tr.visInvisible}</div>
                <div slot="supporting-text">{tr.visInvisibleSub}</div>
              </md-menu-item>
            </md-menu>
          </div>

          {/* Official <md-icon-button> with Library icon (badge ONLY for active transfers!) */}
          <div className="icon-btn-wrap">
            <md-icon-button
              title={tr.transfers}
              aria-label={tr.transfers}
              selected={view === 'transfers' || undefined}
              onClick={() => setView(view === 'transfers' ? 'home' : 'transfers')}
            >
              <Icon name="library" size={21} />
            </md-icon-button>
            {activeTransfersCount > 0 && (
              <span className="icon-badge">{activeTransfersCount}</span>
            )}
          </div>

          {/* Official <md-icon-button> for Settings */}
          <md-icon-button
            title={tr.settings}
            aria-label={tr.settings}
            selected={view === 'settings' || undefined}
            onClick={() => {
              setShowDevInfo(false);
              setView(view === 'settings' ? 'home' : 'settings');
            }}
          >
            <Icon name="settings" size={21} />
          </md-icon-button>
        </div>
      </header>

      {/* ===================== Main Stage ===================== */}
      <main className="stage-scroll">
        <div className="stage-inner">
          {/* Incoming Transfer Banner */}
          {pendingIncoming.map((t) => (
            <section key={t.id} className="incoming-banner" role="alert">
              <div style={{ display: 'flex', alignItems: 'center', gap: 14 }}>
                <div className="device-avatar" style={{ color: 'var(--md-sys-color-tertiary)' }}>
                  <ExpressiveShape kind="clover4" size={50} />
                  <span style={{ color: 'var(--md-sys-color-on-tertiary)', display: 'grid', placeItems: 'center', zIndex: 2 }}>
                    <Icon name="smartphone" size={22} />
                  </span>
                </div>
                <div>
                  <div className="m3-title-large">{tr.wantsToSend(t.peerName)}</div>
                  <div className="m3-body-medium" style={{ opacity: 0.9, marginTop: 2 }}>
                    {t.files.length > 0 ? t.files.join(', ') : tr.filesFallback}
                    {t.totalBytes > 0 ? ` • ${formatBytes(t.totalBytes)}` : ''}
                  </div>
                </div>
              </div>

              <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
                {t.pinCode && (
                  <div className="pin-box">
                    <span className="m3-label-medium" style={{ opacity: 0.7 }}>
                      {tr.pinLabel}
                    </span>
                    <span className="m3-headline-small m3-mono">{t.pinCode}</span>
                  </div>
                )}
                <md-filled-button
                  onClick={async () => {
                    await sendTransferAction(t.id, 'Accept');
                    showSnack(tr.snackTransferAccepted);
                    setView('transfers');
                  }}
                >
                  <span slot="icon"><Icon name="check" size={18} /></span>
                  {tr.accept}
                </md-filled-button>
                <md-filled-tonal-button
                  onClick={async () => {
                    await sendTransferAction(t.id, 'Decline');
                    showSnack(tr.snackTransferDeclined);
                  }}
                >
                  <span slot="icon"><Icon name="close" size={18} /></span>
                  {tr.decline}
                </md-filled-tonal-button>
              </div>
            </section>
          ))}

          {/* ===================== HOME VIEW ===================== */}
          {view === 'home' && (
            <>
              <md-outlined-card className={`hero-drop-card ${dragging ? 'dragging' : ''}`}>
                <div className={`hero-drop-inner ${queuedFiles.length > 0 ? 'compact' : ''}`}>
                  {queuedFiles.length === 0 ? (
                    <>
                      <div className="hero-shape-badge">
                        <ExpressiveShape kind={dragging ? 'softBurst' : 'cookie7'} size={72} />
                        <span style={{ color: 'var(--md-sys-color-on-primary-container)', display: 'grid', placeItems: 'center', zIndex: 2 }}>
                          <Icon name="upload_file" size={30} />
                        </span>
                      </div>

                      <div className="m3-title-large" style={{ marginTop: 4 }}>
                        {tr.dropTitle}
                      </div>

                      <div className="drop-links">
                        <md-text-button onClick={handlePickFiles}>
                          {tr.selectFiles}
                        </md-text-button>
                        <span style={{ color: 'var(--md-sys-color-on-surface-variant)', fontSize: '0.88rem' }}>
                          {tr.or}
                        </span>
                        <md-text-button onClick={handlePickSendFolder}>
                          {tr.selectFolders}
                        </md-text-button>
                      </div>
                    </>
                  ) : (
                    <>
                      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, flexWrap: 'wrap' }}>
                        <div>
                          <div className="m3-title-medium">
                            {tr.queuedItems(queuedFiles.length)}
                          </div>
                          {totalQueueSize > 0 && (
                            <div style={{ fontSize: '0.8rem', color: 'var(--md-sys-color-on-surface-variant)', marginTop: 2 }}>
                              {formatBytes(totalQueueSize)}
                            </div>
                          )}
                        </div>
                        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
                          <md-filled-tonal-button onClick={handlePickFiles}>
                            <span slot="icon"><Icon name="upload_file" size={17} /></span>
                            {tr.filesBtn}
                          </md-filled-tonal-button>
                          <md-filled-tonal-button onClick={handlePickSendFolder}>
                            <span slot="icon"><Icon name="create_new_folder" size={17} /></span>
                            {tr.folderBtn}
                          </md-filled-tonal-button>
                          <md-outlined-button onClick={() => setQueuedFiles([])}>
                            <span slot="icon"><Icon name="delete" size={17} /></span>
                            {tr.clearBtn}
                          </md-outlined-button>
                        </div>
                      </div>

                      <md-chip-set style={{ marginTop: 6 }}>
                        {queuedFiles.map((f) => (
                          <MdFileChip
                            key={f.path}
                            label={
                              f.size > 0 ? `${f.name} (${formatBytes(f.size)})` : f.name
                            }
                            title={f.path}
                            isFolder={f.name.endsWith('/')}
                            onRemove={() =>
                              setQueuedFiles((prev) => prev.filter((x) => x.path !== f.path))
                            }
                          />
                        ))}
                      </md-chip-set>
                    </>
                  )}
                </div>
              </md-outlined-card>

              <md-outlined-card>
                <div className="card-pad">
                  <div className="nearby-header">
                    <span className="m3-title-large">{tr.nearbyDevices}</span>

                    <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
                      {status?.airdrop?.available && status.airdrop.mode === 'exclusive' && (
                        status.airdrop.active ? (
                          <md-filled-tonal-button
                            disabled={airdropBusy || undefined}
                            onClick={async () => {
                              setAirdropBusy(true);
                              try {
                                const res = await setAirDropEnabled(false);
                                if (res.airdrop) {
                                  setStatus((prev) => (prev ? { ...prev, airdrop: res.airdrop } : prev));
                                }
                              } catch {
                                showSnack(tr.airdropToggleFailed);
                              } finally {
                                setAirdropBusy(false);
                              }
                            }}
                          >
                            <span slot="icon"><Icon name="bolt" size={17} /></span>
                            {tr.airdropModeActiveBtn}
                          </md-filled-tonal-button>
                        ) : (
                          <md-outlined-button
                            disabled={airdropBusy || undefined}
                            onClick={async () => {
                              setAirdropBusy(true);
                              try {
                                const res = await setAirDropEnabled(true);
                                if (res.airdrop) {
                                  setStatus((prev) => (prev ? { ...prev, airdrop: res.airdrop } : prev));
                                }
                                if (!res.ok) {
                                  showSnack(res.error || tr.airdropToggleFailed);
                                }
                              } catch {
                                showSnack(tr.airdropToggleFailed);
                              } finally {
                                setAirdropBusy(false);
                              }
                            }}
                          >
                            <span slot="icon"><Icon name="bolt" size={17} /></span>
                            {tr.airdropModeBtn}
                          </md-outlined-button>
                        )
                      )}

                      <div className="radar-reload-wrap">
                        {scanning && (
                          <>
                            <span className="radar-ring" />
                            <span className="radar-ring delay" />
                          </>
                        )}
                        <md-filled-tonal-icon-button
                          title={tr.reloadDevicesTitle}
                          aria-label={tr.reloadDevicesTitle}
                          style={{ position: 'relative', zIndex: 2 }}
                          onClick={() => triggerScanBurst(8000, true)}
                        >
                          {scanning ? (
                            <ContainedLoadingIndicator size={40} bare />
                          ) : (
                            <Icon name="refresh" size={22} />
                          )}
                        </md-filled-tonal-icon-button>
                      </div>
                    </div>
                  </div>

                  {status?.airdrop?.active && status.airdrop.mode === 'exclusive' && (
                    <div
                      className="m3-body-medium"
                      style={{
                        color: 'var(--md-sys-color-on-surface-variant)',
                        marginTop: 8,
                        marginBottom: 4,
                      }}
                    >
                      {tr.airdropExclusiveActiveHint}
                    </div>
                  )}

                  {unifiedDevices.length === 0 ? (
                    <div className="nearby-empty">
                      {scanning ? tr.searchingDevices : tr.noDevicesFound}
                    </div>
                  ) : (
                    <div className="devices-grid">
                      {unifiedDevices.map((ep) => {
                        const visStyle = deviceVisual(ep);
                        return (
                          <button
                            key={ep.id}
                            className="device-tile"
                            onClick={() => handleSendTo(ep)}
                          >
                            <md-ripple />
                            <div className="device-avatar" style={{ color: visStyle.tintBg }}>
                              <ExpressiveShape kind={visStyle.shape} size={50} />
                              <span style={{ color: visStyle.tintFg, display: 'grid', placeItems: 'center', zIndex: 2 }}>
                                <Icon name={visStyle.icon} size={22} />
                              </span>
                            </div>
                            <div style={{ minWidth: 0, flex: 1 }}>
                              <div className="device-name">{ep.name}</div>
                              <div className="device-hint">
                                {queuedFiles.length > 0 ? tr.clickToSend : tr.readyToReceive}
                              </div>
                            </div>
                            <Icon name="send" size={18} />
                          </button>
                        );
                      })}
                    </div>
                  )}
                </div>
              </md-outlined-card>
            </>
          )}

          {/* ===================== TRANSFERS VIEW ===================== */}
          {view === 'transfers' && (
            <>
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                  <md-icon-button onClick={() => setView('home')} title={tr.back}>
                    <Icon name="arrow_back" size={21} />
                  </md-icon-button>
                  <span className="m3-headline-small">{tr.transfers}</span>
                </div>
                <md-filled-tonal-button onClick={() => openPathNative(status?.download_dir || '')}>
                  <span slot="icon"><Icon name="folder_open" size={18} /></span>
                  {tr.openDownloads}
                </md-filled-tonal-button>
              </div>

              {transferList.length === 0 ? (
                <md-outlined-card>
                  <div style={{ textAlign: 'center', padding: '42px 24px' }}>
                    <div className="m3-title-medium">{tr.noTransfersYet}</div>
                    <div className="m3-body-medium" style={{ color: 'var(--md-sys-color-on-surface-variant)', marginTop: 6 }}>
                      {tr.noTransfersSub}
                    </div>
                  </div>
                </md-outlined-card>
              ) : (
                transferList.map((t) => {
                  const isActive =
                    t.state === 'ReceivingFiles' ||
                    t.state === 'SendingFiles' ||
                    t.state === 'Initial' ||
                    t.state === 'SentUkeyClientInit';
                  const pct =
                    t.state === 'Finished'
                      ? 100
                      : t.totalBytes > 0
                        ? Math.round((t.ackBytes / t.totalBytes) * 100)
                        : -1;

                  return (
                    <md-outlined-card key={t.id}>
                      <div className="card-pad" style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
                        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, flexWrap: 'wrap' }}>
                          <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
                            <div
                              className="device-avatar"
                              style={{
                                width: 42,
                                height: 42,
                                color:
                                  t.state === 'Finished'
                                    ? 'var(--md-sys-color-primary-container)'
                                    : 'var(--md-sys-color-secondary-container)',
                              }}
                            >
                              <ExpressiveShape
                                kind={t.state === 'Finished' ? 'clover4' : 'squircle'}
                                size={42}
                              />
                              <span
                                style={{
                                  color:
                                    t.state === 'Finished'
                                      ? 'var(--md-sys-color-on-primary-container)'
                                      : 'var(--md-sys-color-on-secondary-container)',
                                  display: 'grid',
                                  placeItems: 'center',
                                  zIndex: 2,
                                }}
                              >
                                <Icon name={t.state === 'Finished' ? 'check' : 'swap_vert'} size={18} />
                              </span>
                            </div>
                            <div>
                              <div className="m3-title-medium">
                                {t.kind === 'Inbound' ? tr.fromPeer(t.peerName) : tr.toPeer(t.peerName)}
                              </div>
                              <div className="m3-body-medium" style={{ color: 'var(--md-sys-color-on-surface-variant)' }}>
                                {t.files.length > 0 ? t.files.join(', ') : tr.transferFallback}
                              </div>
                            </div>
                          </div>

                          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                            <span className={`status-pill ${t.state === 'Finished' ? 'primary' : ''}`}>
                              {stateLabel(t.state, tr)}
                            </span>
                            {t.state === 'Finished' && (
                              <md-filled-tonal-button
                                onClick={() =>
                                  openPathNative(
                                    t.savedPaths[0] || t.destination || status?.download_dir || ''
                                  )
                                }
                              >
                                <span slot="icon"><Icon name="folder_open" size={17} /></span>
                                {tr.openBtn}
                              </md-filled-tonal-button>
                            )}
                            {isActive && (
                              <md-outlined-button
                                onClick={() => sendTransferAction(t.id, 'Cancel')}
                              >
                                {tr.cancel}
                              </md-outlined-button>
                            )}
                          </div>
                        </div>

                        {(isActive || t.state === 'Finished') && (
                          <>
                            <WavyProgressIndicator progress={pct} active={isActive} />
                            <div
                              className="m3-label-medium m3-mono"
                              style={{
                                display: 'flex',
                                justifyContent: 'space-between',
                                color: 'var(--md-sys-color-on-surface-variant)',
                              }}
                            >
                              <span>
                                {t.totalBytes > 0
                                  ? `${formatBytes(t.state === 'Finished' ? t.totalBytes : t.ackBytes)} / ${formatBytes(t.totalBytes)}`
                                  : stateLabel(t.state, tr)}
                              </span>
                              <span>{pct >= 0 ? `${pct} %` : tr.running}</span>
                            </div>
                          </>
                        )}
                      </div>
                    </md-outlined-card>
                  );
                })
              )}
            </>
          )}

          {/* ===================== SETTINGS VIEW ===================== */}
          {view === 'settings' && (
            <>
              <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                <md-icon-button onClick={() => setView('home')} title={tr.back}>
                  <Icon name="arrow_back" size={21} />
                </md-icon-button>
                <span className="m3-headline-small">{tr.settings}</span>
              </div>

              {/* Receive & Storage */}
              <md-outlined-card>
                <div className="card-pad" style={{ display: 'flex', flexDirection: 'column', gap: 20 }}>
                  <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 16 }}>
                    <div>
                      <div className="m3-title-medium">{tr.autoAcceptTitle}</div>
                      <div className="m3-body-medium" style={{ color: 'var(--md-sys-color-on-surface-variant)' }}>
                        {tr.autoAcceptSub}
                      </div>
                    </div>
                    <MdSwitch
                      selected={Boolean(status?.auto_accept)}
                      onChange={handleToggleAutoAccept}
                      ariaLabel={tr.autoAcceptTitle}
                    />
                  </div>

                  <md-divider />

                  <div>
                    <div className="m3-title-medium" style={{ marginBottom: 10 }}>
                      {tr.downloadDirTitle}
                    </div>
                    <div style={{ display: 'flex', gap: 10, alignItems: 'center' }}>
                      <md-outlined-text-field
                        read-only
                        value={status?.download_dir || '~/Downloads'}
                        style={{ flex: 1 }}
                      />
                      <md-filled-tonal-button onClick={handlePickDownloadDir}>
                        <span slot="icon"><Icon name="folder_open" size={18} /></span>
                        {tr.changeBtn}
                      </md-filled-tonal-button>
                    </div>
                  </div>
                </div>
              </md-outlined-card>

              {/* Language, Appearance & Official Dynamic Color Scheme */}
              <md-outlined-card>
                <div className="card-pad" style={{ display: 'flex', flexDirection: 'column', gap: 20 }}>
                  <div>
                    <div className="m3-title-medium" style={{ marginBottom: 12 }}>
                      {tr.languageTitle}
                    </div>
                    <md-outlined-segmented-button-set>
                      <md-outlined-segmented-button
                        label={tr.langSystem}
                        selected={langPref === 'system' || undefined}
                        onClick={() => setLangPref('system')}
                      />
                      <md-outlined-segmented-button
                        label={tr.langDe}
                        selected={langPref === 'de' || undefined}
                        onClick={() => setLangPref('de')}
                      />
                      <md-outlined-segmented-button
                        label={tr.langEn}
                        selected={langPref === 'en' || undefined}
                        onClick={() => setLangPref('en')}
                      />
                    </md-outlined-segmented-button-set>
                  </div>

                  <md-divider />

                  <div>
                    <div className="m3-title-medium" style={{ marginBottom: 12 }}>
                      {tr.appearanceTitle}
                    </div>
                    <md-outlined-segmented-button-set>
                      <md-outlined-segmented-button
                        label={tr.modeSystem}
                        selected={themeMode === 'system' || undefined}
                        onClick={() => setThemeMode('system')}
                      />
                      <md-outlined-segmented-button
                        label={tr.modeLight}
                        selected={themeMode === 'light' || undefined}
                        onClick={() => setThemeMode('light')}
                      />
                      <md-outlined-segmented-button
                        label={tr.modeDark}
                        selected={themeMode === 'dark' || undefined}
                        onClick={() => setThemeMode('dark')}
                      />
                    </md-outlined-segmented-button-set>
                  </div>

                  <md-divider />

                  <div>
                    <div className="m3-title-medium">{tr.colorThemeTitle}</div>
                    <div className="m3-body-medium" style={{ color: 'var(--md-sys-color-on-surface-variant)', marginTop: 2 }}>
                      {seed === 'system'
                        ? tr.colorFollowsSystem
                        : THEME_SEEDS.find((x) => x.id === seed)?.label}
                    </div>
                    <div className="swatch-row">
                      <button
                        className="swatch-btn"
                        title={tr.colorSystemTooltip}
                        onClick={() => setSeed('system')}
                        style={{ color: status?.system_accent || '#526B92' }}
                      >
                        <md-ripple />
                        <ExpressiveShape
                          kind={seed === 'system' ? 'softBurst' : 'squircle'}
                          size={seed === 'system' ? 52 : 44}
                        />
                        <span style={{ color: '#ffffff', display: 'grid', placeItems: 'center', zIndex: 2 }}>
                          <Icon name={seed === 'system' ? 'check' : 'sparkle'} size={19} />
                        </span>
                      </button>

                      {THEME_SEEDS.map((s) => {
                        const active = seed === s.id;
                        return (
                          <button
                            key={s.id}
                            className="swatch-btn"
                            title={s.label}
                            onClick={() => setSeed(s.id)}
                            style={{ color: s.hex }}
                          >
                            <md-ripple />
                            <ExpressiveShape
                              kind={active ? 'cookie7' : 'squircle'}
                              size={active ? 50 : 44}
                            />
                            {active && (
                              <span style={{ color: '#ffffff', display: 'grid', placeItems: 'center', zIndex: 2 }}>
                                <Icon name="check" size={20} />
                              </span>
                            )}
                          </button>
                        );
                      })}
                    </div>
                  </div>
                </div>
              </md-outlined-card>

              {/* About & Version */}
              <md-outlined-card>
                <div
                  className="card-pad"
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                    gap: 16,
                    flexWrap: 'wrap',
                  }}
                >
                  <div>
                    <div className="m3-title-medium">{tr.aboutTitle}</div>
                    <div
                      className="m3-body-medium"
                      style={{ color: 'var(--md-sys-color-on-surface-variant)', marginTop: 2 }}
                    >
                      {updateInfo.state === 'checking'
                        ? tr.checkingUpdates
                        : updateInfo.state === 'available' && updateInfo.latestVersion
                          ? tr.updateAvailable(updateInfo.latestVersion)
                          : updateInfo.state === 'latest'
                            ? tr.upToDate(status?.version || '0.1.0')
                            : updateInfo.state === 'error'
                              ? tr.updateCheckFailed
                              : tr.versionLabel(status?.version || '—')}
                    </div>
                  </div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                    {updateInfo.state === 'available' && updateInfo.releaseUrl && (
                      <md-filled-button onClick={() => openPathNative(updateInfo.releaseUrl!)}>
                        <span slot="icon"><Icon name="sparkle" size={18} /></span>
                        {tr.openReleaseBtn}
                      </md-filled-button>
                    )}
                    <md-filled-tonal-button
                      disabled={updateInfo.state === 'checking' || undefined}
                      onClick={handleCheckUpdates}
                    >
                      <span slot="icon"><Icon name="refresh" size={18} /></span>
                      {tr.checkUpdatesBtn}
                    </md-filled-tonal-button>
                  </div>
                </div>
              </md-outlined-card>

              {/* Hidden Developer Information Button */}
              <div style={{ display: 'flex', justifyContent: 'flex-start' }}>
                <md-outlined-button
                  onClick={() => {
                    if (!showDevInfo) refreshStatus();
                    setShowDevInfo((v) => !v);
                  }}
                >
                  <span slot="icon"><Icon name="code" size={18} /></span>
                  {showDevInfo ? tr.devInfoHide : tr.devInfoShow}
                </md-outlined-button>
              </div>

              {showDevInfo && (
                <md-outlined-card>
                  <div className="card-pad">
                    <div className="m3-title-medium" style={{ marginBottom: 12 }}>
                      {tr.devInfoTitle}
                    </div>
                    <div className="diag-row">
                      <span className="m3-label-large">{tr.devVersion}</span>
                      <span className="m3-mono">v{status?.version || '—'}</span>
                    </div>
                    <div className="diag-row">
                      <span className="m3-label-large">{tr.devQsPort}</span>
                      <span className="m3-mono">{status?.qs_port ?? '—'}</span>
                    </div>
                    <div className="diag-row">
                      <span className="m3-label-large">{tr.devAdPort}</span>
                      <span className="m3-mono">{status?.airdrop_port ?? 8771}</span>
                    </div>
                    <div className="diag-row">
                      <span className="m3-label-large">{tr.devAirDropRadio}</span>
                      <span className="m3-mono">
                        {status?.airdrop
                          ? `${status.airdrop.mode}${status.airdrop.active ? ' (active)' : ''}${status.airdrop.driver ? ` · ${status.airdrop.driver}` : ''}${status.airdrop.reason ? ` — ${status.airdrop.reason}` : ''}`
                          : '—'}
                      </span>
                    </div>
                    <div className="diag-row">
                      <span className="m3-label-large">{tr.devActiveIps}</span>
                      <span className="m3-mono">
                        {status?.active_lan_ips?.join(', ') || tr.devNone}
                      </span>
                    </div>
                    <div className="diag-row">
                      <span className="m3-label-large">{tr.devSystemAccent}</span>
                      <span className="m3-mono">
                        {status?.system_accent || '—'} ({status?.system_color_scheme || 'auto'})
                      </span>
                    </div>

                    <div className="m3-label-large" style={{ margin: '14px 0 8px' }}>
                      {tr.devInterfaces}
                    </div>
                    {(status?.interfaces || []).map((ifc, idx) => (
                      <div key={`${ifc.name}-${ifc.ip}-${idx}`} className="diag-row">
                        <div>
                          <div className="m3-mono" style={{ fontSize: '0.84rem', fontWeight: 700 }}>
                            {ifc.name} ({ifc.ip})
                          </div>
                          <div style={{ fontSize: '0.76rem', color: 'var(--md-sys-color-on-surface-variant)' }}>
                            {ifc.reason}
                          </div>
                        </div>
                        <span className={`status-pill ${ifc.allowed ? 'primary' : ''}`}>
                          <Icon name={ifc.allowed ? 'check' : 'shield'} size={13} />
                          {ifc.allowed ? tr.devActive : tr.devFiltered}
                        </span>
                      </div>
                    ))}
                  </div>
                </md-outlined-card>
              )}
            </>
          )}
        </div>
      </main>

      {snack && (
        <div className="m3-snackbar" role="status">
          <Icon name="sparkle" size={15} />
          <span>{snack}</span>
        </div>
      )}
    </div>
  );
}
