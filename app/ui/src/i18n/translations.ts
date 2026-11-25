export type LangCode = 'de' | 'en';
export type LangPref = 'system' | 'de' | 'en';

export interface Translations {
  // Visibility
  visVisible: string;
  visVisibleSub: string;
  visTemp: string;
  visTempSub: string;
  visInvisible: string;
  visInvisibleSub: string;

  // Top bar & navigation
  transfers: string;
  settings: string;
  back: string;

  // Home dropzone
  dropTitle: string;
  selectFiles: string;
  or: string;
  selectFolders: string;
  queuedItems: (count: number) => string;
  filesBtn: string;
  folderBtn: string;
  clearBtn: string;

  // Nearby devices
  nearbyDevices: string;
  reloadDevicesTitle: string;
  searchingDevices: string;
  noDevicesFound: string;
  clickToSend: string;
  readyToReceive: string;
  airdropModeBtn: string;
  airdropModeActiveBtn: string;
  airdropExclusiveActiveHint: string;
  airdropToggleFailed: string;
  devAirDropRadio: string;

  // Incoming transfer sheet
  wantsToSend: (peer: string) => string;
  filesFallback: string;
  unknownDevice: string;
  deviceFallback: string;
  pinLabel: string;
  accept: string;
  decline: string;
  cancel: string;

  // Transfers view
  openDownloads: string;
  noTransfersYet: string;
  noTransfersSub: string;
  fromPeer: (peer: string) => string;
  toPeer: (peer: string) => string;
  transferFallback: string;
  openBtn: string;
  running: string;

  // Transfer states
  stateWaiting: string;
  stateReceiving: string;
  stateSending: string;
  stateFinished: string;
  stateRejected: string;
  stateCancelled: string;
  stateDisconnected: string;
  stateConnecting: string;

  // Settings view
  autoAcceptTitle: string;
  autoAcceptSub: string;
  downloadDirTitle: string;
  changeBtn: string;
  appearanceTitle: string;
  modeSystem: string;
  modeLight: string;
  modeDark: string;
  languageTitle: string;
  langSystem: string;
  langDe: string;
  langEn: string;
  colorThemeTitle: string;
  colorFollowsSystem: string;
  colorSystemTooltip: string;
  devInfoShow: string;
  devInfoHide: string;
  devInfoTitle: string;
  devVersion: string;
  devQsPort: string;
  devAdPort: string;
  devActiveIps: string;
  devNone: string;
  devSystemAccent: string;
  devInterfaces: string;
  devActive: string;
  devFiltered: string;
  aboutTitle: string;
  versionLabel: (v: string) => string;
  checkUpdatesBtn: string;
  checkingUpdates: string;
  upToDate: (v: string) => string;
  updateAvailable: (v: string) => string;
  openReleaseBtn: string;
  updateCheckFailed: string;

  // Snackbars
  snackItemsAdded: (count: number) => string;
  snackFilesAdded: (count: number) => string;
  snackPickFilesFailed: string;
  snackPickFolderFailed: string;
  snackSendingTo: (name: string) => string;
  snackSendFailed: string;
  snackFolderUpdated: string;
  snackAutoAcceptOn: string;
  snackAutoAcceptOff: string;
  snackTransferAccepted: string;
  snackTransferDeclined: string;
}

const DE: Translations = {
  visVisible: 'Für alle sichtbar',
  visVisibleSub: 'Geräte in der Nähe können senden',
  visTemp: 'Kurzzeitig sichtbar',
  visTempSub: 'Vorübergehend empfangsbereit',
  visInvisible: 'Unsichtbar',
  visInvisibleSub: 'Niemand sieht dieses Gerät',

  transfers: 'Übertragungen',
  settings: 'Einstellungen',
  back: 'Zurück',

  dropTitle: 'Dateien oder Ordner zum Senden hinzufügen',
  selectFiles: 'Dateien auswählen',
  or: 'oder',
  selectFolders: 'Ordner auswählen',
  queuedItems: (n) => (n === 1 ? '1 Element bereit zum Senden' : `${n} Elemente bereit zum Senden`),
  filesBtn: 'Dateien',
  folderBtn: 'Ordner',
  clearBtn: 'Leeren',

  nearbyDevices: 'Geräte in der Nähe',
  reloadDevicesTitle: 'Erneut nach Geräten suchen',
  searchingDevices: 'Suche nach empfangsbereiten Geräten in der Nähe…',
  noDevicesFound: 'Keine erreichbaren Geräte gefunden.',
  clickToSend: 'Klicken zum Senden',
  readyToReceive: 'Bereit zum Empfangen',
  airdropModeBtn: 'AirDrop',
  airdropModeActiveBtn: 'AirDrop aktiv',
  airdropExclusiveActiveHint:
    'AirDrop-Modus aktiv (WLAN ist kurz pausiert und verbindet sich beim Ausschalten automatisch neu).',
  airdropToggleFailed: 'AirDrop-Modus konnte nicht umgeschaltet werden',
  devAirDropRadio: 'AirDrop AWDL-Radio',

  wantsToSend: (peer) => `${peer} möchte senden`,
  filesFallback: 'Dateien',
  unknownDevice: 'Unbekanntes Gerät',
  deviceFallback: 'Gerät',
  pinLabel: 'PIN',
  accept: 'Annehmen',
  decline: 'Ablehnen',
  cancel: 'Abbrechen',

  openDownloads: 'Downloads öffnen',
  noTransfersYet: 'Noch keine Übertragungen',
  noTransfersSub: 'Gesendete und empfangene Dateien erscheinen hier.',
  fromPeer: (peer) => `Von ${peer}`,
  toPeer: (peer) => `An ${peer}`,
  transferFallback: 'Übertragung',
  openBtn: 'Öffnen',
  running: 'Läuft…',

  stateWaiting: 'Wartet auf Bestätigung',
  stateReceiving: 'Wird empfangen…',
  stateSending: 'Wird gesendet…',
  stateFinished: 'Abgeschlossen',
  stateRejected: 'Abgelehnt',
  stateCancelled: 'Abgebrochen',
  stateDisconnected: 'Getrennt',
  stateConnecting: 'Verbindet…',

  autoAcceptTitle: 'Dateien automatisch annehmen',
  autoAcceptSub: 'Eingehende Dateien direkt ohne Bestätigung speichern',
  downloadDirTitle: 'Speicherort für empfangene Dateien',
  changeBtn: 'Ändern',
  appearanceTitle: 'Erscheinungsbild',
  modeSystem: 'System',
  modeLight: 'Hell',
  modeDark: 'Dunkel',
  languageTitle: 'Sprache',
  langSystem: 'System',
  langDe: 'Deutsch',
  langEn: 'English',
  colorThemeTitle: 'Farbwelt',
  colorFollowsSystem: 'Folgt automatisch deiner System-Akzentfarbe',
  colorSystemTooltip: 'System-Akzentfarbe (automatisch)',
  devInfoShow: 'Entwicklerinformationen',
  devInfoHide: 'Entwicklerinformationen ausblenden',
  devInfoTitle: 'Entwicklerinformationen',
  devVersion: 'App-Version',
  devQsPort: 'Quick Share TCP-Port',
  devAdPort: 'AirDrop HTTPS-Port',
  devActiveIps: 'Aktive LAN-IPs',
  devNone: 'Keine',
  devSystemAccent: 'System-Akzentfarbe',
  devInterfaces: 'Netzwerk-Interfaces',
  devActive: 'Aktiv',
  devFiltered: 'Gefiltert',
  aboutTitle: 'Über AetherShare',
  versionLabel: (v) => `Version ${v}`,
  checkUpdatesBtn: 'Nach Updates suchen',
  checkingUpdates: 'Prüfe auf Updates…',
  upToDate: (v) => `Aktuell (v${v})`,
  updateAvailable: (v) => `Update verfügbar: v${v}`,
  openReleaseBtn: 'Release öffnen',
  updateCheckFailed: 'Update-Prüfung fehlgeschlagen',

  snackItemsAdded: (n) => (n === 1 ? '1 Element hinzugefügt' : `${n} Elemente hinzugefügt`),
  snackFilesAdded: (n) => (n === 1 ? '1 Datei hinzugefügt' : `${n} Dateien hinzugefügt`),
  snackPickFilesFailed: 'Dateiauswahl fehlgeschlagen',
  snackPickFolderFailed: 'Ordnerauswahl fehlgeschlagen',
  snackSendingTo: (name) => `Sende an ${name}…`,
  snackSendFailed: 'Senden konnte nicht gestartet werden',
  snackFolderUpdated: 'Zielordner aktualisiert',
  snackAutoAcceptOn: 'Automatisch annehmen aktiv',
  snackAutoAcceptOff: 'Automatisch annehmen aus',
  snackTransferAccepted: 'Transfer angenommen',
  snackTransferDeclined: 'Transfer abgelehnt',
};

const EN: Translations = {
  visVisible: 'Visible to everyone',
  visVisibleSub: 'Nearby devices can send to you',
  visTemp: 'Temporarily visible',
  visTempSub: 'Temporarily ready to receive',
  visInvisible: 'Invisible',
  visInvisibleSub: 'No one can see this device',

  transfers: 'Transfers',
  settings: 'Settings',
  back: 'Back',

  dropTitle: 'Add files or folders to send',
  selectFiles: 'Select files',
  or: 'or',
  selectFolders: 'Select folders',
  queuedItems: (n) => (n === 1 ? '1 item ready to send' : `${n} items ready to send`),
  filesBtn: 'Files',
  folderBtn: 'Folder',
  clearBtn: 'Clear',

  nearbyDevices: 'Nearby devices',
  reloadDevicesTitle: 'Search for devices again',
  searchingDevices: 'Looking for nearby devices…',
  noDevicesFound: 'No reachable devices found.',
  clickToSend: 'Click to send',
  readyToReceive: 'Ready to receive',
  airdropModeBtn: 'AirDrop',
  airdropModeActiveBtn: 'AirDrop active',
  airdropExclusiveActiveHint:
    'AirDrop mode active (Wi-Fi is paused and reconnects automatically when turned off).',
  airdropToggleFailed: 'Could not toggle AirDrop mode',
  devAirDropRadio: 'AirDrop AWDL radio',

  wantsToSend: (peer) => `${peer} wants to share`,
  filesFallback: 'Files',
  unknownDevice: 'Unknown device',
  deviceFallback: 'Device',
  pinLabel: 'PIN',
  accept: 'Accept',
  decline: 'Decline',
  cancel: 'Cancel',

  openDownloads: 'Open Downloads',
  noTransfersYet: 'No transfers yet',
  noTransfersSub: 'Sent and received files will appear here.',
  fromPeer: (peer) => `From ${peer}`,
  toPeer: (peer) => `To ${peer}`,
  transferFallback: 'Transfer',
  openBtn: 'Open',
  running: 'In progress…',

  stateWaiting: 'Waiting for confirmation',
  stateReceiving: 'Receiving…',
  stateSending: 'Sending…',
  stateFinished: 'Completed',
  stateRejected: 'Declined',
  stateCancelled: 'Cancelled',
  stateDisconnected: 'Disconnected',
  stateConnecting: 'Connecting…',

  autoAcceptTitle: 'Auto-accept files',
  autoAcceptSub: 'Save incoming files immediately without asking',
  downloadDirTitle: 'Download folder',
  changeBtn: 'Change',
  appearanceTitle: 'Appearance',
  modeSystem: 'System',
  modeLight: 'Light',
  modeDark: 'Dark',
  languageTitle: 'Language',
  langSystem: 'System',
  langDe: 'Deutsch',
  langEn: 'English',
  colorThemeTitle: 'Color theme',
  colorFollowsSystem: 'Follows your desktop accent color automatically',
  colorSystemTooltip: 'System accent color (automatic)',
  devInfoShow: 'Developer information',
  devInfoHide: 'Hide developer information',
  devInfoTitle: 'Developer information',
  devVersion: 'App version',
  devQsPort: 'Quick Share TCP port',
  devAdPort: 'AirDrop HTTPS port',
  devActiveIps: 'Active LAN IPs',
  devNone: 'None',
  devSystemAccent: 'System accent color',
  devInterfaces: 'Network interfaces',
  devActive: 'Active',
  devFiltered: 'Filtered',
  aboutTitle: 'About AetherShare',
  versionLabel: (v) => `Version ${v}`,
  checkUpdatesBtn: 'Check for updates',
  checkingUpdates: 'Checking for updates…',
  upToDate: (v) => `Up to date (v${v})`,
  updateAvailable: (v) => `Update available: v${v}`,
  openReleaseBtn: 'Open release',
  updateCheckFailed: 'Could not check for updates',

  snackItemsAdded: (n) => (n === 1 ? '1 item added' : `${n} items added`),
  snackFilesAdded: (n) => (n === 1 ? '1 file added' : `${n} files added`),
  snackPickFilesFailed: 'Could not open file picker',
  snackPickFolderFailed: 'Could not open folder picker',
  snackSendingTo: (name) => `Sending to ${name}…`,
  snackSendFailed: 'Could not start transfer',
  snackFolderUpdated: 'Download folder updated',
  snackAutoAcceptOn: 'Auto-accept enabled',
  snackAutoAcceptOff: 'Auto-accept disabled',
  snackTransferAccepted: 'Transfer accepted',
  snackTransferDeclined: 'Transfer declined',
};

export function resolveLang(
  pref: LangPref,
  backendSystemLang?: string | null
): LangCode {
  if (pref === 'de') return 'de';
  if (pref === 'en') return 'en';

  // Prefer backend DE-aware locale detection (KDE plasma-localerc, GNOME/XDG LC_*/LANGUAGE/LANG)
  if (backendSystemLang === 'de') return 'de';
  if (backendSystemLang === 'en') return 'en';

  // Browser / WebView fallback before first /api/status response
  const navLangs =
    navigator.languages && navigator.languages.length > 0
      ? navigator.languages
      : [navigator.language || 'en'];
  for (const l of navLangs) {
    const base = l.toLowerCase().split(/[-_]/)[0];
    if (base === 'de') return 'de';
    if (base === 'en') return 'en';
  }
  return 'en';
}

export function getTranslations(lang: LangCode): Translations {
  return lang === 'de' ? DE : EN;
}
