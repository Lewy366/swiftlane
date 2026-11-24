// BIOS update USB: detect motherboard, inspect BIOS archive/image, copy to FAT32 USB stick, and verify by SHA-256 read-back.
(() => {
  if (typeof I18N !== 'undefined') {
    Object.assign(I18N.de, {
      'BIOS update USB': 'BIOS-Update-USB',
      'Prepare and verify a FAT32 USB stick for a motherboard BIOS update': 'FAT32-USB-Stick für ein Mainboard-BIOS-Update vorbereiten und prüfen',
      'Detect your motherboard, verify the BIOS file, and copy it safely to a FAT32 USB stick.': 'Mainboard erkennen, BIOS-Datei prüfen und sicher auf einen FAT32-USB-Stick kopieren.',
      '1. Your motherboard': '1. Dein Mainboard',
      '2. BIOS file': '2. BIOS-Datei',
      '3. USB stick & flash mode': '3. USB-Stick & Flash-Modus',
      '4. Ready to flash': '4. Bereit zum Flashen',
      'Refresh': 'Aktualisieren',
      'Scanning…': 'Suche…',
      'Motherboard': 'Mainboard',
      'Revision': 'Revision',
      'Current BIOS': 'Aktuelles BIOS',
      'Target vendor': 'Ziel-Hersteller',
      'Not detected': 'Nicht erkannt',
      'Not reported': 'Nicht gemeldet',
      'Copy model': 'Modell kopieren',
      'Model copied': 'Modell kopiert',
      'SHA-256 copied': 'SHA-256 kopiert',
      'Drop the BIOS .zip or image file here': 'BIOS-.zip oder Bilddatei hier ablegen',
      'or click to browse · .zip, .cap, .rom, .bin, .bio and vendor BIOS extensions': 'oder klicken zum Auswählen · .zip, .cap, .rom, .bin, .bio und Hersteller-Endungen',
      'File inside zip': 'Datei im Archiv',
      'Looks valid': 'Sieht gültig aus',
      'Check warnings': 'Hinweise prüfen',
      'Inspecting BIOS file…': 'Prüfe BIOS-Datei…',
      'In-BIOS updater': 'Im-BIOS-Updater',
      'Flashback button': 'Flashback-Taste',
      'Flash mode': 'Flash-Modus',
      'Removable USB stick': 'Wechseldatenträger (USB)',
      'Refresh drives': 'Laufwerke aktualisieren',
      'Copy & verify on USB': 'Kopieren & auf USB prüfen',
      'Copying & verifying…': 'Kopiere & prüfe…',
      'Safe eject USB': 'USB sicher auswerfen',
      'Ejecting…': 'Werfe aus…',
      'Safe to remove': 'Kann sicher entfernt werden',
      'Safe to remove USB stick': 'USB-Stick kann sicher entfernt werden',
      'Format as FAT32 (Windows)…': 'Als FAT32 formatieren (Windows)…',
      'Open Disk Management': 'Datenträgerverwaltung öffnen',
      'Rename to': 'Umbenennen in',
      'Empty': 'Leer',
      'Next steps': 'Nächste Schritte',
      'Never turn off or unplug your PC while BIOS is updating. Wait until it restarts on its own.': 'Schalte den PC während des BIOS-Updates niemals aus und ziehe nicht den Stecker. Warte, bis er von selbst neu startet.',
      'BitLocker is active on C:. Back up your recovery key (or suspend BitLocker) before updating BIOS, as flashing can trigger the recovery screen.': 'BitLocker ist auf C: aktiv. Sichere deinen Wiederherstellungsschlüssel (oder pausiere BitLocker) vor dem BIOS-Update, da das Flashen den Wiederherstellungsbildschirm auslösen kann.',
      'This looks like a laptop or prebuilt PC. Most laptops use a Windows installer from the manufacturer instead of a BIOS USB stick — keep AC power plugged in.': 'Dies sieht nach einem Laptop oder Fertig-PC aus. Die meisten Laptops nutzen ein Windows-Installationsprogramm des Herstellers statt eines BIOS-USB-Sticks — Netzteil angeschlossen lassen.',
      "Board revision couldn't be read reliably (common on Gigabyte boards). Check the printed REV number on the corner of your motherboard before downloading.": 'Die Mainboard-Revision konnte nicht zuverlässig gelesen werden (häufig bei Gigabyte-Boards). Prüfe die aufgedruckte REV-Nummer in der Ecke des Mainboards vor dem Download.',
      'BIOSRenamer.exe inside the zip was ignored for safety — Swiftlane never runs downloaded executables.': 'BIOSRenamer.exe im Archiv wurde zur Sicherheit ignoriert — Swiftlane führt niemals heruntergeladene Programme aus.',
      'Confirm this filename in your motherboard manual — some boards use a different name.': 'Bestätige diesen Dateinamen im Handbuch deines Mainboards — manche Boards nutzen einen anderen Namen.',
      "Enter the short filename shown on your ASUS support page (for example, TUF650P.CAP), or run ASUS's BIOSRenamer yourself and drop the renamed .CAP file above.": 'Gib den kurzen Dateinamen von deiner ASUS-Supportseite ein (z. B. TUF650P.CAP) oder führe BIOSRenamer selbst aus und ziehe die umbenannte .CAP-Datei oben hinein.',
      'Enter the exact filename required by your motherboard manual for rear-button Flashback.': 'Gib den genauen Dateinamen ein, den das Handbuch deines Mainboards für Flashback verlangt.',
      'No removable USB sticks found. Plug in a FAT32 USB stick and click Refresh drives. Fixed and system drives are never listed.': 'Keine Wechseldatenträger gefunden. Stecke einen FAT32-USB-Stick ein und klicke auf „Laufwerke aktualisieren“. Feste Laufwerke und Systemlaufwerke werden nie aufgelistet.',
      'This USB stick already has files on it. An empty USB stick is recommended for Flashback — Swiftlane never deletes your existing files.': 'Auf diesem USB-Stick sind bereits Dateien. Für Flashback wird ein leerer Stick empfohlen — Swiftlane löscht niemals deine vorhandenen Dateien.',
      'Flashback buttons almost always require an MBR partition table, not GPT. Convert the stick to MBR in Disk Management if flashing fails.': 'Flashback-Tasten erfordern fast immer eine MBR-Partitionstabelle statt GPT. Wandle den Stick in der Datenträgerverwaltung in MBR um, falls das Flashen fehlschlägt.',
      "Windows' Format dialog cannot format drives larger than 32 GB as FAT32. Use a 32 GB or smaller USB stick, or create a smaller FAT32 partition in Disk Management.": 'Der Windows-Formatierungsdialog kann Laufwerke über 32 GB nicht als FAT32 formatieren. Nutze einen Stick mit höchstens 32 GB oder erstelle in der Datenträgerverwaltung eine kleinere FAT32-Partition.',
      'Fixed and system drives are never shown.': 'Feste Laufwerke und Systemlaufwerke werden nie angezeigt.',
    });
    Object.assign(I18N.pl, {
      'BIOS update USB': 'Pendrive aktualizacji BIOS',
      'Prepare and verify a FAT32 USB stick for a motherboard BIOS update': 'Przygotuj i zweryfikuj pendrive FAT32 do aktualizacji BIOS płyty głównej',
      'Detect your motherboard, verify the BIOS file, and copy it safely to a FAT32 USB stick.': 'Wykryj płytę główną, zweryfikuj plik BIOS i bezpiecznie skopiuj go na pendrive FAT32.',
      '1. Your motherboard': '1. Twoja płyta główna',
      '2. BIOS file': '2. Plik BIOS',
      '3. USB stick & flash mode': '3. Pendrive USB i tryb flashowania',
      '4. Ready to flash': '4. Gotowe do flashowania',
      'Refresh': 'Odśwież',
      'Scanning…': 'Skanowanie…',
      'Motherboard': 'Płyta główna',
      'Revision': 'Rewizja',
      'Current BIOS': 'Obecny BIOS',
      'Target vendor': 'Producent docelowy',
      'Not detected': 'Nie wykryto',
      'Not reported': 'Brak danych',
      'Copy model': 'Kopiuj model',
      'Model copied': 'Skopiowano model',
      'SHA-256 copied': 'Skopiowano SHA-256',
      'Drop the BIOS .zip or image file here': 'Upuść tutaj archiwum .zip lub plik BIOS',
      'or click to browse · .zip, .cap, .rom, .bin, .bio and vendor BIOS extensions': 'lub kliknij, aby wybrać · .zip, .cap, .rom, .bin, .bio i rozszerzenia producentów',
      'File inside zip': 'Plik w archiwum',
      'Looks valid': 'Wygląda poprawnie',
      'Check warnings': 'Sprawdź ostrzeżenia',
      'Inspecting BIOS file…': 'Sprawdzanie pliku BIOS…',
      'In-BIOS updater': 'Aktualizator w BIOS',
      'Flashback button': 'Przycisk Flashback',
      'Flash mode': 'Tryb flashowania',
      'Removable USB stick': 'Pendrive USB',
      'Refresh drives': 'Odśwież dyski',
      'Copy & verify on USB': 'Skopiuj i zweryfikuj na USB',
      'Copying & verifying…': 'Kopiowanie i weryfikacja…',
      'Safe eject USB': 'Bezpiecznie wysuń USB',
      'Ejecting…': 'Wysuwanie…',
      'Safe to remove': 'Można bezpiecznie odłączyć',
      'Safe to remove USB stick': 'Można bezpiecznie odłączyć pendrive USB',
      'Format as FAT32 (Windows)…': 'Formatuj jako FAT32 (Windows)…',
      'Open Disk Management': 'Otwórz Zarządzanie dyskami',
      'Rename to': 'Zmień nazwę na',
      'Empty': 'Pusty',
      'Next steps': 'Następne kroki',
      'Never turn off or unplug your PC while BIOS is updating. Wait until it restarts on its own.': 'Nigdy nie wyłączaj ani nie odłączaj komputera podczas aktualizacji BIOS. Poczekaj, aż sam uruchomi się ponownie.',
      'BitLocker is active on C:. Back up your recovery key (or suspend BitLocker) before updating BIOS, as flashing can trigger the recovery screen.': 'Funkcja BitLocker jest aktywna na dysku C:. Zabezpiecz klucz odzyskiwania (lub wstrzymaj BitLocker) przed aktualizacją BIOS, ponieważ może ona wywołać ekran odzyskiwania.',
      'This looks like a laptop or prebuilt PC. Most laptops use a Windows installer from the manufacturer instead of a BIOS USB stick — keep AC power plugged in.': 'To wygląda na laptop lub gotowy komputer. Większość laptopów używa instalatora producenta z systemu Windows zamiast pendrive’a BIOS — pozostaw podłączony zasilacz.',
      "Board revision couldn't be read reliably (common on Gigabyte boards). Check the printed REV number on the corner of your motherboard before downloading.": 'Nie udało się pewnie odczytać rewizji płyty (częste w płytach Gigabyte). Przed pobraniem sprawdź numer REV nadrukowany w rogu płyty głównej.',
      'BIOSRenamer.exe inside the zip was ignored for safety — Swiftlane never runs downloaded executables.': 'Plik BIOSRenamer.exe w archiwum został zignorowany ze względów bezpieczeństwa — Swiftlane nigdy nie uruchamia pobranych plików wykonywalnych.',
      'Confirm this filename in your motherboard manual — some boards use a different name.': 'Potwierdź tę nazwę pliku w instrukcji płyty głównej — niektóre modele używają innej nazwy.',
      "Enter the short filename shown on your ASUS support page (for example, TUF650P.CAP), or run ASUS's BIOSRenamer yourself and drop the renamed .CAP file above.": 'Wpisz krótką nazwę pliku ze strony wsparcia ASUS (np. TUF650P.CAP) albo uruchom samodzielnie BIOSRenamer i upuść zmieniony plik .CAP powyżej.',
      'Enter the exact filename required by your motherboard manual for rear-button Flashback.': 'Wpisz dokładną nazwę pliku wymaganą przez instrukcję płyty głównej dla przycisku Flashback.',
      'No removable USB sticks found. Plug in a FAT32 USB stick and click Refresh drives. Fixed and system drives are never listed.': 'Nie znaleziono wymiennych pendrive’ów USB. Podłącz pendrive FAT32 i kliknij „Odśwież dyski”. Dyski stałe i systemowe nigdy nie są wyświetlane.',
      'This USB stick already has files on it. An empty USB stick is recommended for Flashback — Swiftlane never deletes your existing files.': 'Na tym pendrivie są już pliki. Do Flashback zalecany jest pusty pendrive — Swiftlane nigdy nie usuwa istniejących plików.',
      'Flashback buttons almost always require an MBR partition table, not GPT. Convert the stick to MBR in Disk Management if flashing fails.': 'Przyciski Flashback niemal zawsze wymagają tabeli partycji MBR, a nie GPT. Jeśli flashowanie się nie powiedzie, przekonwertuj pendrive na MBR w Zarządzaniu dyskami.',
      "Windows' Format dialog cannot format drives larger than 32 GB as FAT32. Use a 32 GB or smaller USB stick, or create a smaller FAT32 partition in Disk Management.": 'Okno formatowania Windows nie pozwala sformatować dysku powyżej 32 GB jako FAT32. Użyj pendrive’a do 32 GB lub utwórz mniejszą partycję FAT32 w Zarządzaniu dyskami.',
      'Fixed and system drives are never shown.': 'Dyski stałe i systemowe nigdy nie są pokazywane.',
    });
    Object.assign(I18N.it, {
      'BIOS update USB': 'Chiavetta USB BIOS',
      'Prepare and verify a FAT32 USB stick for a motherboard BIOS update': 'Prepara e verifica una chiavetta USB FAT32 per aggiornare il BIOS della scheda madre',
      'Detect your motherboard, verify the BIOS file, and copy it safely to a FAT32 USB stick.': 'Rileva la scheda madre, verifica il file BIOS e copialo in sicurezza su una chiavetta USB FAT32.',
      '1. Your motherboard': '1. La tua scheda madre',
      '2. BIOS file': '2. File BIOS',
      '3. USB stick & flash mode': '3. Chiavetta USB e modalità flash',
      '4. Ready to flash': '4. Pronto per il flash',
      'Refresh': 'Aggiorna',
      'Scanning…': 'Rilevamento…',
      'Motherboard': 'Scheda madre',
      'Revision': 'Revisione',
      'Current BIOS': 'BIOS attuale',
      'Target vendor': 'Produttore di destinazione',
      'Not detected': 'Non rilevata',
      'Not reported': 'Non indicata',
      'Copy model': 'Copia modello',
      'Model copied': 'Modello copiato',
      'SHA-256 copied': 'SHA-256 copiato',
      'Drop the BIOS .zip or image file here': 'Trascina qui lo .zip o il file BIOS',
      'or click to browse · .zip, .cap, .rom, .bin, .bio and vendor BIOS extensions': 'oppure clicca per scegliere · .zip, .cap, .rom, .bin, .bio ed estensioni dei produttori',
      'File inside zip': 'File nello zip',
      'Looks valid': 'Sembra valido',
      'Check warnings': 'Controlla gli avvisi',
      'Inspecting BIOS file…': 'Verifica file BIOS…',
      'In-BIOS updater': 'Updater nel BIOS',
      'Flashback button': 'Tasto Flashback',
      'Flash mode': 'Modalità flash',
      'Removable USB stick': 'Chiavetta USB rimovibile',
      'Refresh drives': 'Aggiorna unità',
      'Copy & verify on USB': 'Copia e verifica su USB',
      'Copying & verifying…': 'Copia e verifica…',
      'Safe eject USB': 'Espelli USB in sicurezza',
      'Ejecting…': 'Espulsione…',
      'Safe to remove': 'Puoi rimuoverla in sicurezza',
      'Safe to remove USB stick': 'Ora puoi rimuovere la chiavetta USB in sicurezza',
      'Format as FAT32 (Windows)…': 'Formatta come FAT32 (Windows)…',
      'Open Disk Management': 'Apri Gestione disco',
      'Rename to': 'Rinomina in',
      'Empty': 'Vuota',
      'Next steps': 'Prossimi passaggi',
      'Never turn off or unplug your PC while BIOS is updating. Wait until it restarts on its own.': 'Non spegnere né scollegare mai il PC durante l’aggiornamento del BIOS. Attendi che si riavvii da solo.',
      'BitLocker is active on C:. Back up your recovery key (or suspend BitLocker) before updating BIOS, as flashing can trigger the recovery screen.': 'BitLocker è attivo su C:. Salva la chiave di ripristino (o sospendi BitLocker) prima di aggiornare il BIOS, perché il flash può attivare la schermata di ripristino.',
      'This looks like a laptop or prebuilt PC. Most laptops use a Windows installer from the manufacturer instead of a BIOS USB stick — keep AC power plugged in.': 'Sembra un portatile o un PC preassemblato. La maggior parte dei portatili usa un programma Windows del produttore invece di una chiavetta USB BIOS — tieni l’alimentatore collegato.',
      "Board revision couldn't be read reliably (common on Gigabyte boards). Check the printed REV number on the corner of your motherboard before downloading.": 'La revisione della scheda non è leggibile con certezza (comune sulle schede Gigabyte). Controlla il numero REV stampato sull’angolo della scheda madre prima di scaricare.',
      'BIOSRenamer.exe inside the zip was ignored for safety — Swiftlane never runs downloaded executables.': 'BIOSRenamer.exe nello zip è stato ignorato per sicurezza — Swiftlane non esegue mai programmi scaricati.',
      'Confirm this filename in your motherboard manual — some boards use a different name.': 'Conferma questo nome file nel manuale della scheda madre — alcuni modelli usano un nome diverso.',
      "Enter the short filename shown on your ASUS support page (for example, TUF650P.CAP), or run ASUS's BIOSRenamer yourself and drop the renamed .CAP file above.": 'Inserisci il nome breve indicato nella pagina di supporto ASUS (ad esempio TUF650P.CAP), oppure avvia tu stesso BIOSRenamer e trascina sopra il file .CAP rinominato.',
      'Enter the exact filename required by your motherboard manual for rear-button Flashback.': 'Inserisci il nome file esatto richiesto dal manuale della scheda madre per il Flashback dal tasto posteriore.',
      'No removable USB sticks found. Plug in a FAT32 USB stick and click Refresh drives. Fixed and system drives are never listed.': 'Nessuna chiavetta USB rimovibile trovata. Collega una chiavetta FAT32 e clicca su Aggiorna unità. I dischi fissi e di sistema non vengono mai mostrati.',
      'This USB stick already has files on it. An empty USB stick is recommended for Flashback — Swiftlane never deletes your existing files.': 'Questa chiavetta USB contiene già dei file. Per il Flashback è consigliata una chiavetta vuota — Swiftlane non elimina mai i tuoi file.',
      'Flashback buttons almost always require an MBR partition table, not GPT. Convert the stick to MBR in Disk Management if flashing fails.': 'I tasti Flashback richiedono quasi sempre una tabella di partizioni MBR, non GPT. Se il flash non riesce, converti la chiavetta in MBR da Gestione disco.',
      "Windows' Format dialog cannot format drives larger than 32 GB as FAT32. Use a 32 GB or smaller USB stick, or create a smaller FAT32 partition in Disk Management.": 'La finestra Formatta di Windows non può formattare in FAT32 unità oltre 32 GB. Usa una chiavetta da 32 GB o meno, oppure crea una partizione FAT32 più piccola in Gestione disco.',
      'Fixed and system drives are never shown.': 'I dischi fissi e di sistema non vengono mai mostrati.',
    });
  }

  const FALLBACK_VENDORS = [
    { id: 'asus', label: 'ASUS', support_url: 'https://www.asus.com/support/download-center/', in_bios_tool: 'EZ Flash', flashback_name: 'BIOS FlashBack', flashback_filename: '', bios_key: 'Del or F2' },
    { id: 'msi', label: 'MSI', support_url: 'https://www.msi.com/support/download/', in_bios_tool: 'M-Flash', flashback_name: 'Flash BIOS Button', flashback_filename: 'MSI.ROM', bios_key: 'Del' },
    { id: 'gigabyte', label: 'Gigabyte', support_url: 'https://www.gigabyte.com/Support', in_bios_tool: 'Q-Flash', flashback_name: 'Q-Flash Plus', flashback_filename: 'GIGABYTE.bin', bios_key: 'Del or End' },
    { id: 'asrock', label: 'ASRock', support_url: 'https://www.asrock.com/support/', in_bios_tool: 'Instant Flash', flashback_name: 'BIOS Flashback', flashback_filename: 'CREATIVE.ROM', bios_key: 'F2 or Del' },
    { id: 'biostar', label: 'BIOSTAR', support_url: 'https://www.biostar.com.tw/app/en/support/download.php', in_bios_tool: 'BIOS-FLASHER', flashback_name: 'SMART BIOS UPDATE', flashback_filename: '', bios_key: 'Del or F12' },
    { id: 'other', label: 'Other / Custom', support_url: '', in_bios_tool: 'In-BIOS updater', flashback_name: 'BIOS Flashback', flashback_filename: '', bios_key: 'Del or F2' },
  ];

  const RESERVED_WIN = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\.|$)/i;

  let board = null;
  let vendors = [...FALLBACK_VENDORS];
  let selectedVendorId = 'other';
  let currentPath = '';
  let inspected = null;
  let drives = [];
  let selectedDriveLetter = '';
  let mode = 'in_bios';
  let customName = '';
  let prepared = null;

  let detecting = false;
  let inspecting = false;
  let loadingDrives = false;
  let formatting = false;
  let preparing = false;
  let ejecting = false;

  function currentVendor() {
    return vendors.find(v => v.id === selectedVendorId) || vendors[vendors.length - 1] || FALLBACK_VENDORS[FALLBACK_VENDORS.length - 1];
  }

  function boardDisplayName() {
    if (!board) return '';
    const maker = String(board.maker || '').trim();
    const model = String(board.model || '').trim();
    if (!maker) return model;
    if (!model) return maker;
    if (model.toLowerCase().startsWith(maker.toLowerCase())) return model;
    return `${maker} ${model}`;
  }

  function needsCustomName() {
    if (mode !== 'flashback') return false;
    const v = currentVendor();
    return v.id === 'asus' || !v.flashback_filename;
  }

  function validateCustomName(raw) {
    const val = String(raw || '').trim();
    if (!val) return { ok: false, reason: '' };
    if (val.length > 64) return { ok: false, reason: t('Filename is too long (max 64 characters).') };
    if (/[\\/:*?"<>|\x00-\x1f]/.test(val) || val.includes('..')) {
      return { ok: false, reason: t('Enter a plain filename only (no folder path or invalid characters).') };
    }
    if (val.startsWith('.') || val.endsWith('.') || RESERVED_WIN.test(val)) {
      return { ok: false, reason: t('Enter a valid Windows filename.') };
    }
    return { ok: true, value: val };
  }

  function targetFilename() {
    if (mode === 'flashback') {
      if (needsCustomName()) return customName.trim();
      return currentVendor().flashback_filename || '';
    }
    return inspected ? (inspected.entry_name || inspected.source_name || '') : '';
  }

  function selectedDrive() {
    return drives.find(d => d.letter === selectedDriveLetter) || null;
  }

  function warnText(val, fallback) {
    if (!val) return null;
    if (typeof val === 'string' && val.trim()) return val.trim();
    return fallback;
  }

  async function copyText(str, okMsg) {
    if (!str) return;
    try {
      if (App.api && App.api.copy_text) await App.api.copy_text(str);
      else if (navigator.clipboard) await navigator.clipboard.writeText(str);
      toast(t(okMsg));
    } catch (_) {
      toast(t('Could not copy to clipboard'));
    }
  }

  function makeNotice(text, opts) {
    const box = el('div', 'notice bios-notice');
    box.append(icon('info'));
    const body = el('div', 'bios-notice-body');
    const span = el('span', null, text);
    if (opts && opts.raw) span.setAttribute('translate', 'no');
    body.append(span);
    if (opts && opts.actionLabel && opts.onAction) {
      const acts = el('div', 'bios-notice-acts');
      const btn = el('button', 'btn small', opts.actionLabel);
      btn.type = 'button';
      if (opts.disabled) btn.disabled = true;
      btn.onclick = opts.onAction;
      acts.append(btn);
      body.append(acts);
    }
    box.append(body);
    return box;
  }

  function makeInfoNote(text) {
    const box = el('div', 'bios-info-note');
    box.append(icon('info'), el('span', null, text));
    return box;
  }

  // ---------- Step 1: Motherboard detection
  function renderBoard() {
    const btn = $('biosRefreshBoard');
    if (btn) {
      btn.disabled = detecting;
      btn.replaceChildren(icon('refresh'), ' ' + t(detecting ? 'Scanning…' : 'Refresh'));
    }

    const name = boardDisplayName();
    const nameEl = $('biosBoardName');
    nameEl.textContent = detecting && !board ? t('Scanning…') : (name || t('Not detected'));
    nameEl.setAttribute('translate', name ? 'no' : 'yes');

    const revEl = $('biosBoardRev');
    const rev = board && board.revision ? String(board.revision).trim() : '';
    revEl.textContent = rev || t('Not reported');
    revEl.setAttribute('translate', rev ? 'no' : 'yes');

    const biosEl = $('biosCurrentBios');
    const bVer = board && board.bios_version ? String(board.bios_version).trim() : '';
    const bDate = board && board.bios_date ? String(board.bios_date).trim() : '';
    const biosStr = [bVer, bDate].filter(Boolean).join(' · ');
    biosEl.textContent = biosStr || t('Not reported');
    biosEl.setAttribute('translate', biosStr ? 'no' : 'yes');

    const sel = $('biosVendor');
    sel.replaceChildren();
    for (const v of vendors) {
      const opt = el('option', null, v.label);
      opt.value = v.id;
      sel.append(opt);
    }
    if (!vendors.some(v => v.id === selectedVendorId) && vendors.length) {
      selectedVendorId = vendors[0].id;
    }
    sel.value = selectedVendorId;

    const copyBtn = $('biosCopyModel');
    copyBtn.disabled = !name;

    const v = currentVendor();
    const supportBtn = $('biosOpenSupport');
    const supportUrl = v.support_url || (board && selectedVendorId === board.vendor ? board.support_url : '');
    supportBtn.hidden = !supportUrl;
    supportBtn.replaceChildren(icon('open'), ' ' + (v.id === 'other' ? t('Open support page') : `Open ${v.label} support page`));
    supportBtn.dataset.url = supportUrl || '';

    const notices = $('biosBoardNotices');
    notices.replaceChildren();
    if (board) {
      const bl = board.bitlocker && String(board.bitlocker.status || '').toLowerCase();
      if (bl === 'on' || bl === 'encrypting' || bl === 'locked') {
        notices.append(makeNotice(t('BitLocker is active on C:. Back up your recovery key (or suspend BitLocker) before updating BIOS, as flashing can trigger the recovery screen.')));
      }
      if (board.is_laptop || board.is_oem_prebuilt) {
        notices.append(makeNotice(t('This looks like a laptop or prebuilt PC. Most laptops use a Windows installer from the manufacturer instead of a BIOS USB stick — keep AC power plugged in.')));
      }
      if (board.revision_uncertain) {
        notices.append(makeNotice(t("Board revision couldn't be read reliably (common on Gigabyte boards). Check the printed REV number on the corner of your motherboard before downloading.")));
      }
    }
    notices.hidden = notices.children.length === 0;
  }

  async function detectBoard() {
    if (detecting) return;
    detecting = true;
    renderBoard();
    try {
      const r = await App.api.biosusb_detect();
      if (r && !r.error) {
        board = r;
        if (Array.isArray(r.vendors) && r.vendors.length) {
          vendors = r.vendors;
        }
        if (r.vendor && vendors.some(v => v.id === r.vendor)) {
          selectedVendorId = r.vendor;
        } else if (vendors.length) {
          selectedVendorId = vendors[vendors.length - 1].id;
        }
      } else if (r && r.error) {
        toast(r.error);
      }
    } catch (err) {
      toast(String(err));
    } finally {
      detecting = false;
      renderBoard();
      renderMode();
      renderDrives();
      renderAction();
      if (prepared) renderReady();
    }
  }

  // ---------- Step 2: Inspect BIOS file / zip
  function clearFile() {
    currentPath = '';
    inspected = null;
    prepared = null;
    $('biosFileErr').hidden = true;
    $('biosFileErr').textContent = '';
    renderFile();
    renderMode();
    renderAction();
    renderReady();
  }

  function renderFile() {
    const infoWrap = $('biosFileInfo');
    const clearBtn = $('biosClearFile');
    const busyEl = $('biosInspectBusy');

    busyEl.hidden = !inspecting;
    clearBtn.hidden = !inspected && !currentPath;

    if (!inspected) {
      infoWrap.hidden = true;
      infoWrap.replaceChildren();
      return;
    }

    infoWrap.hidden = false;
    infoWrap.replaceChildren();

    const hasWarnings = !!(inspected.size_warning || inspected.header_warning || inspected.board_mismatch || inspected.looks_valid === false);

    // Top summary row
    const head = el('div', 'bios-file-head');
    const titleWrap = el('div', 'bios-file-title');
    const fileNameEl = el('b', 'bios-mono', inspected.entry_name || inspected.source_name || baseName(currentPath));
    fileNameEl.setAttribute('translate', 'no');
    titleWrap.append(fileNameEl);

    if (inspected.source_kind === 'zip' && inspected.source_name) {
      const fromEl = el('span', 'muted small bios-from');
      fromEl.append(document.createTextNode(t('from') + ' '));
      const srcCode = el('span', 'bios-mono', inspected.source_name);
      srcCode.setAttribute('translate', 'no');
      fromEl.append(srcCode);
      titleWrap.append(fromEl);
    }

    const pills = el('div', 'bios-pills');
    const sizePill = el('span', 'bios-pill bios-mono', fmtSize(inspected.size || 0));
    sizePill.setAttribute('translate', 'no');
    pills.append(sizePill);

    const statusPill = el('span', 'bios-pill ' + (hasWarnings ? 'bios-pill-warn' : 'bios-pill-ok'), t(hasWarnings ? 'Check warnings' : 'Looks valid'));
    pills.append(statusPill);

    head.append(titleWrap, pills);
    infoWrap.append(head);

    // Multiple candidate selector (zip with >1 possible BIOS file)
    if (Array.isArray(inspected.candidates) && inspected.candidates.length > 1) {
      const candRow = el('div', 'bios-cand-row');
      candRow.append(el('span', 'muted small', t('File inside zip')));
      const sel = el('select', 'input bios-cand-sel');
      sel.setAttribute('translate', 'no');
      for (const c of inspected.candidates) {
        const opt = el('option', null, `${c.entry} (${fmtSize(c.size || 0)})`);
        opt.value = c.entry;
        sel.append(opt);
      }
      sel.value = inspected.entry_name || inspected.candidates[0].entry;
      sel.onchange = () => inspectFile(currentPath, sel.value);
      candRow.append(sel);
      infoWrap.append(candRow);
    }

    // Full SHA-256 display + Copy button
    if (inspected.sha256) {
      const hashBox = el('div', 'bios-hash-box');
      const hashHead = el('div', 'bios-hash-head');
      hashHead.append(el('span', 'label', 'SHA-256'));
      const copyHashBtn = el('button', 'btn ghost small');
      copyHashBtn.type = 'button';
      copyHashBtn.append(icon('copy'), ' ' + t('Copy'));
      copyHashBtn.onclick = () => copyText(inspected.sha256, 'SHA-256 copied');
      hashHead.append(copyHashBtn);

      const code = el('code', 'bios-hash-code', inspected.sha256);
      code.setAttribute('translate', 'no');
      hashBox.append(hashHead, code);
      infoWrap.append(hashBox);
    }

    // Soft warnings & info notes (never blocking)
    const notes = el('div', 'bios-stack');
    const sizeW = warnText(inspected.size_warning, t('This file is larger than 65 MB — double-check that you selected the BIOS image and not a driver or utility package.'));
    if (sizeW) notes.append(makeNotice(sizeW, { raw: typeof inspected.size_warning === 'string' }));

    const headW = warnText(inspected.header_warning, t("The file header doesn't look like a typical UEFI/BIOS image. Confirm it came from your motherboard's support page."));
    if (headW) notes.append(makeNotice(headW, { raw: typeof inspected.header_warning === 'string' }));

    const misW = warnText(inspected.board_mismatch, t("The filename doesn't appear to match your motherboard model. Double-check before flashing."));
    if (misW) notes.append(makeNotice(misW, { raw: typeof inspected.board_mismatch === 'string' }));

    if (inspected.has_bios_renamer) {
      notes.append(makeInfoNote(t('BIOSRenamer.exe inside the zip was ignored for safety — Swiftlane never runs downloaded executables.')));
    }

    if (notes.children.length) infoWrap.append(notes);
  }

  async function inspectFile(path, entryName) {
    if (!path || inspecting) return;
    inspecting = true;
    currentPath = path;
    prepared = null;
    $('biosFileErr').hidden = true;
    $('biosFileErr').textContent = '';
    renderFile();
    renderReady();
    try {
      const vendorId = selectedVendorId || (board && board.vendor) || '';
      const boardModel = (board && (!selectedVendorId || selectedVendorId === board.vendor)) ? (board.model || '') : '';
      const r = entryName
        ? await App.api.biosusb_inspect(path, vendorId, boardModel, entryName)
        : await App.api.biosusb_inspect(path, vendorId, boardModel);
      if (!r || r.error) {
        inspected = null;
        $('biosFileErr').textContent = (r && r.error) || t('Could not inspect this file.');
        $('biosFileErr').hidden = false;
      } else {
        inspected = r;
        // If ASUS user already ran BIOSRenamer and dropped a short .CAP file directly, prefill customName gently.
        if (selectedVendorId === 'asus' && !customName && r.source_kind !== 'zip' && r.entry_name && /\.cap$/i.test(r.entry_name) && r.entry_name.length <= 20) {
          customName = r.entry_name;
          $('biosCustomName').value = customName;
        }
      }
    } catch (err) {
      inspected = null;
      $('biosFileErr').textContent = String(err);
      $('biosFileErr').hidden = false;
    } finally {
      inspecting = false;
      renderFile();
      renderMode();
      renderAction();
    }
  }

  // ---------- Step 3: Flash mode & removable USB drives
  function renderMode() {
    const v = currentVendor();
    const seg = $('biosModeSeg');
    seg.querySelectorAll('button').forEach(b => b.classList.toggle('on', b.dataset.v === mode));

    const detail = $('biosModeDetail');
    detail.replaceChildren();

    const customWrap = $('biosCustomWrap');
    const customInput = $('biosCustomName');
    const customHint = $('biosCustomHint');
    const customErr = $('biosCustomErr');

    if (mode === 'in_bios') {
      customWrap.hidden = true;
      customErr.hidden = true;
      const toolName = v.in_bios_tool || t('In-BIOS updater');
      const p = el('p', 'muted small bios-mode-text');
      const b = el('b', null, toolName);
      b.setAttribute('translate', 'no');
      p.append(b, document.createTextNode(' — ' + t('keeps the original BIOS filename on the root of the USB stick')));
      if (inspected && (inspected.entry_name || inspected.source_name)) {
        p.append(document.createTextNode(' ('));
        const fn = el('code', 'bios-inline-code', inspected.entry_name || inspected.source_name);
        fn.setAttribute('translate', 'no');
        p.append(fn, document.createTextNode(').'));
      } else {
        p.append(document.createTextNode('.'));
      }
      detail.append(p);
    } else {
      const fbName = v.flashback_name || t('BIOS Flashback');
      if (needsCustomName()) {
        const p = el('p', 'muted small bios-mode-text');
        const b = el('b', null, fbName);
        b.setAttribute('translate', 'no');
        p.append(b, document.createTextNode(' — ' + t('copies the BIOS image to the USB root using the exact short filename your board expects.')));
        detail.append(p);

        customWrap.hidden = false;
        customInput.placeholder = v.id === 'asus' ? 'e.g. TUF650P.CAP' : 'e.g. BIOS.ROM';
        customHint.textContent = v.id === 'asus'
          ? t("Enter the short filename shown on your ASUS support page (for example, TUF650P.CAP), or run ASUS's BIOSRenamer yourself and drop the renamed .CAP file above.")
          : t('Enter the exact filename required by your motherboard manual for rear-button Flashback.');

        const check = validateCustomName(customName);
        if (customName.trim() && !check.ok) {
          customErr.textContent = check.reason;
          customErr.hidden = false;
        } else {
          customErr.hidden = true;
        }
      } else {
        customWrap.hidden = true;
        customErr.hidden = true;
        const p = el('p', 'muted small bios-mode-text');
        const b = el('b', null, fbName);
        b.setAttribute('translate', 'no');
        const fn = el('code', 'bios-inline-code', v.flashback_filename);
        fn.setAttribute('translate', 'no');
        p.append(b, document.createTextNode(' — ' + t('will save as') + ' '), fn, document.createTextNode('. ' + t('Confirm this filename in your motherboard manual — some boards use a different name.')));
        detail.append(p);
      }
    }
  }

  function renderDrives() {
    const refreshBtn = $('biosRefreshDrives');
    if (refreshBtn) {
      refreshBtn.disabled = loadingDrives;
      refreshBtn.replaceChildren(icon('refresh'), ' ' + t(loadingDrives ? 'Scanning…' : 'Refresh drives'));
    }

    const list = $('biosDrives');
    const empty = $('biosDrivesEmpty');
    const help = $('biosDriveHelp');
    list.replaceChildren();
    help.replaceChildren();

    if (!drives.length) {
      empty.hidden = false;
      help.hidden = true;
      selectedDriveLetter = '';
      renderAction();
      return;
    }

    empty.hidden = true;
    if (!drives.some(d => d.letter === selectedDriveLetter)) {
      const firstOk = drives.find(d => d.fat32_ok);
      selectedDriveLetter = (firstOk || drives[0]).letter;
    }

    for (const d of drives) {
      const on = d.letter === selectedDriveLetter;
      const card = el('button', 'bios-drive' + (on ? ' on' : ''));
      card.type = 'button';

      const letter = el('div', 'bios-drive-letter', d.letter);
      letter.setAttribute('translate', 'no');

      const main = el('div', 'bios-drive-main');
      const top = el('div', 'bios-drive-top');
      const label = el('b', null, d.label || t('Removable USB'));
      if (d.label) label.setAttribute('translate', 'no');
      const cap = el('span', 'bios-mono muted small', `${fmtSize(d.total_bytes || 0)} · ${fmtSize(d.free_bytes || 0)} ${t('free')}`);
      cap.setAttribute('translate', 'no');
      top.append(label, cap);

      const badges = el('div', 'bios-pills');
      const fsText = d.fs || 'RAW';
      const fsPill = el('span', 'bios-pill bios-mono ' + (d.fat32_ok ? 'bios-pill-ok' : 'bios-pill-warn'), fsText);
      fsPill.setAttribute('translate', 'no');
      badges.append(fsPill);

      if (d.partition_style) {
        const isGptWarn = mode === 'flashback' && String(d.partition_style).toUpperCase() === 'GPT';
        const ptPill = el('span', 'bios-pill bios-mono' + (isGptWarn ? ' bios-pill-warn' : ''), String(d.partition_style).toUpperCase());
        ptPill.setAttribute('translate', 'no');
        badges.append(ptPill);
      }

      const countText = !d.file_count
        ? t('Empty')
        : `${d.file_count} ${t(d.file_count === 1 ? 'item in root' : 'items in root')}`;
      badges.append(el('span', 'bios-pill', countText));

      main.append(top, badges);
      card.append(letter, main);
      card.onclick = () => {
        selectedDriveLetter = d.letter;
        renderDrives();
        renderAction();
      };
      list.append(card);
    }

    // Contextual helpers for the selected drive
    const sel = selectedDrive();
    if (sel) {
      if (!sel.fat32_ok) {
        if (sel.can_gui_format_fat32) {
          help.append(makeNotice(
            `${sel.letter} is formatted as ${sel.fs || 'an unsupported filesystem'}. Motherboards require FAT32. Formatting erases everything on this USB stick.`,
            {
              actionLabel: t(formatting ? 'Opening Format…' : 'Format as FAT32 (Windows)…'),
              disabled: formatting,
              onAction: () => formatDrive(sel.letter),
            }
          ));
        } else if (sel.over_32gb) {
          help.append(makeNotice(
            t("Windows' Format dialog cannot format drives larger than 32 GB as FAT32. Use a 32 GB or smaller USB stick, or create a smaller FAT32 partition in Disk Management."),
            {
              actionLabel: t('Open Disk Management'),
              onAction: openDiskMgmt,
            }
          ));
        } else {
          help.append(makeNotice(
            `${sel.letter} is formatted as ${sel.fs || 'an unsupported filesystem'}. Motherboards require FAT32.`,
            {
              actionLabel: t('Open Disk Management'),
              onAction: openDiskMgmt,
            }
          ));
        }
      }

      if (mode === 'flashback' && String(sel.partition_style || '').toUpperCase() === 'GPT') {
        help.append(makeNotice(
          t('Flashback buttons almost always require an MBR partition table, not GPT. Convert the stick to MBR in Disk Management if flashing fails.'),
          {
            actionLabel: t('Open Disk Management'),
            onAction: openDiskMgmt,
          }
        ));
      }

      if (mode === 'flashback' && sel.file_count > 0) {
        help.append(makeInfoNote(t('This USB stick already has files on it. An empty USB stick is recommended for Flashback — Swiftlane never deletes your existing files.')));
      }
    }

    help.hidden = help.children.length === 0;
  }

  async function loadDrives() {
    if (loadingDrives) return;
    loadingDrives = true;
    renderDrives();
    try {
      const r = await App.api.biosusb_drives();
      if (r && r.error) {
        toast(r.error);
      } else {
        drives = (r && Array.isArray(r.drives)) ? r.drives : [];
      }
    } catch (err) {
      toast(String(err));
    } finally {
      loadingDrives = false;
      renderDrives();
      renderAction();
    }
  }

  async function formatDrive(letter) {
    if (formatting) return;
    formatting = true;
    renderDrives();
    try {
      const r = await App.api.biosusb_format(letter);
      if (r && r.error) toast(r.error);
    } catch (err) {
      toast(String(err));
    } finally {
      formatting = false;
      await loadDrives();
    }
  }

  async function openDiskMgmt() {
    try {
      const r = await App.api.biosusb_open_diskmgmt();
      if (r && r.error) toast(r.error);
    } catch (err) {
      toast(String(err));
    }
  }

  function renderAction() {
    const btn = $('biosPrepare');
    const summary = $('biosActionSummary');
    const sel = selectedDrive();
    const target = targetFilename();
    const customCheck = needsCustomName() ? validateCustomName(customName) : { ok: true };

    const ready = !!(
      !preparing &&
      inspected &&
      inspected.token &&
      sel &&
      sel.fat32_ok &&
      customCheck.ok &&
      target
    );

    btn.disabled = !ready;
    btn.textContent = t(preparing ? 'Copying & verifying…' : 'Copy & verify on USB');

    summary.replaceChildren();
    if (ready) {
      const root = sel.root || (sel.letter.endsWith('\\') ? sel.letter : sel.letter + '\\');
      summary.append(document.createTextNode(t('Will write') + ' '));
      const dest = el('code', 'bios-inline-code', root + target);
      dest.setAttribute('translate', 'no');
      summary.append(dest, document.createTextNode(' ' + t('and verify SHA-256 directly from the stick.')));
    } else if (!inspected) {
      summary.textContent = t('Drop or select a BIOS file in Step 2 first.');
    } else if (!sel) {
      summary.textContent = t('Plug in a removable FAT32 USB stick to continue.');
    } else if (!sel.fat32_ok) {
      summary.textContent = t('Selected USB stick must be formatted as FAT32.');
    } else if (needsCustomName() && !customCheck.ok) {
      summary.textContent = customCheck.reason || t('Enter the Flashback target filename above.');
    }
  }

  async function prepareUsb(overwrite) {
    const sel = selectedDrive();
    if (!inspected || !inspected.token || !sel || !sel.fat32_ok || preparing) return;

    const customCheck = needsCustomName() ? validateCustomName(customName) : { ok: true, value: null };
    if (!customCheck.ok) {
      renderMode();
      renderAction();
      return;
    }

    preparing = true;
    $('biosPrepErr').hidden = true;
    $('biosPrepErr').textContent = '';
    renderAction();

    try {
      const payload = {
        token: inspected.token,
        drive: sel.letter,
        mode,
        vendor: selectedVendorId,
        custom_name: needsCustomName() ? customCheck.value : null,
        overwrite: !!overwrite,
      };
      const r = await App.api.biosusb_prepare(payload);
      if (r && r.needs_overwrite) {
        preparing = false;
        renderAction();
        const name = r.target_name || targetFilename();
        if (confirm(`${t('A file named')} "${name}" ${t('already exists on this USB stick. Replacing it overwrites the old file immediately — it will be gone even if the copy or verification fails. Replace it?')}`)) {
          await prepareUsb(true);
        }
        return;
      }
      if (!r || r.error) {
        $('biosPrepErr').textContent = (r && r.error) || t('Could not copy BIOS file to USB stick.');
        $('biosPrepErr').hidden = false;
        toast((r && r.error) || t('Copy failed'));
        return;
      }
      prepared = {
        target_path: r.target_path,
        target_name: r.target_name,
        size: r.size,
        sha256: r.sha256,
        verified_unbuffered: r.verified_unbuffered,
        drive: sel.letter,
        mode,
        vendorId: selectedVendorId,
        ejected: false,
      };
      toast(t('Copied and verified on USB'));
      renderReady();
      await loadDrives();
      const step4 = $('biosStep4');
      if (step4) step4.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    } catch (err) {
      $('biosPrepErr').textContent = String(err);
      $('biosPrepErr').hidden = false;
      toast(String(err));
    } finally {
      preparing = false;
      renderAction();
    }
  }

  // ---------- Step 4: Ready to flash
  function renderReady() {
    const card = $('biosStep4');
    if (!prepared) {
      card.hidden = true;
      return;
    }
    card.hidden = false;

    const v = vendors.find(x => x.id === prepared.vendorId) || currentVendor();
    const headline = $('biosVerifiedTitle');
    headline.replaceChildren();
    headline.append(document.createTextNode(t('Copied to') + ' '));
    const pathEl = el('code', 'bios-inline-code', prepared.target_path || (prepared.drive + '\\' + prepared.target_name));
    pathEl.setAttribute('translate', 'no');
    headline.append(
      pathEl,
      document.createTextNode(' · ' + t(prepared.verified_unbuffered === false ? 'SHA-256 verified from USB' : 'SHA-256 verified directly from USB'))
    );

    const meta = $('biosVerifiedMeta');
    meta.textContent = `${fmtSize(prepared.size || 0)} · SHA-256 ${prepared.sha256 || ''}`;
    meta.setAttribute('translate', 'no');

    const ejectBtn = $('biosEject');
    ejectBtn.disabled = ejecting || prepared.ejected;
    ejectBtn.textContent = t(prepared.ejected ? 'Safe to remove' : ejecting ? 'Ejecting…' : 'Safe eject USB');

    const steps = $('biosStepsList');
    steps.replaceChildren();
    const targetName = prepared.target_name || targetFilename();
    const biosKey = v.bios_key || 'Del or F2';
    const inBiosTool = v.in_bios_tool || 'In-BIOS updater';
    const fbTool = v.flashback_name || 'BIOS Flashback';

    if (prepared.mode === 'flashback') {
      const li1 = el('li');
      li1.append(document.createTextNode(t('Click') + ' '), el('b', null, t('Safe eject USB')), document.createTextNode(' ' + t('above and unplug the USB stick.')));

      const li2 = el('li', null, t('Shut down the target PC completely, leaving the power supply switched on and plugged in.'));

      const li3 = el('li');
      li3.append(document.createTextNode(t('Plug the USB stick into the dedicated rear I/O USB port labelled') + ' '));
      const portCode = el('code', 'bios-inline-code', 'BIOS / Flash BIOS');
      portCode.setAttribute('translate', 'no');
      li3.append(portCode, document.createTextNode(' ' + t('(check your motherboard manual — regular USB ports will not trigger Flashback).')));

      const li4 = el('li');
      li4.append(document.createTextNode(t('Press and hold the') + ' '));
      const fbEl = el('b', null, fbTool);
      fbEl.setAttribute('translate', 'no');
      li4.append(fbEl, document.createTextNode(' ' + t('button for about 3 seconds until the LED starts flashing, then wait until it stops completely (usually 5–8 minutes) before powering on.')));

      steps.append(li1, li2, li3, li4);
    } else {
      const li1 = el('li', null, t('Leave the USB stick plugged into a rear motherboard USB port (or safely eject it if flashing another PC).'));

      const li2 = el('li');
      li2.append(document.createTextNode(t('Restart the PC and repeatedly tap') + ' '));
      const keyEl = el('code', 'bios-inline-code', biosKey);
      keyEl.setAttribute('translate', 'no');
      li2.append(keyEl, document.createTextNode(' ' + t('to enter UEFI / BIOS Setup.')));

      const li3 = el('li');
      li3.append(document.createTextNode(t('Open') + ' '));
      const toolEl = el('b', null, inBiosTool);
      toolEl.setAttribute('translate', 'no');
      li3.append(toolEl, document.createTextNode(', ' + t('select') + ' '));
      const fnEl = el('code', 'bios-inline-code', targetName);
      fnEl.setAttribute('translate', 'no');
      li3.append(fnEl, document.createTextNode(' ' + t('on the USB stick, and confirm the update.')));

      const li4 = el('li', null, t('Wait until the PC finishes and restarts on its own. It may reboot 2–3 times and take 1–2 minutes for memory training.'));

      steps.append(li1, li2, li3, li4);
    }
  }

  async function ejectUsb() {
    if (!prepared || ejecting || prepared.ejected) return;
    ejecting = true;
    renderReady();
    try {
      const r = await App.api.biosusb_eject(prepared.drive);
      if (r && r.error) {
        toast(r.error);
      } else {
        prepared.ejected = true;
        toast(t('Safe to remove USB stick'));
        await loadDrives();
      }
    } catch (err) {
      toast(String(err));
    } finally {
      ejecting = false;
      renderReady();
    }
  }

  // ---------- page registration
  App.pages.biosusb = {
    init() {
      $('biosRefreshBoard').onclick = detectBoard;
      $('biosCopyModel').onclick = () => copyText(boardDisplayName(), 'Model copied');
      $('biosOpenSupport').onclick = async () => {
        const url = $('biosOpenSupport').dataset.url;
        if (!url) return;
        const r = await App.api.biosusb_open_url(url);
        if (r && r.error) toast(r.error);
      };

      $('biosVendor').onchange = () => {
        selectedVendorId = $('biosVendor').value;
        renderBoard();
        renderMode();
        renderDrives();
        renderAction();
        if (currentPath && inspected) {
          inspectFile(currentPath, inspected.entry_name || undefined);
        }
      };

      $('biosDrop').onclick = async () => {
        if (inspecting) return;
        try {
          const r = await App.api.biosusb_pick_file();
          if (r && r.path) await inspectFile(r.path);
          else if (r && r.error) toast(r.error);
        } catch (err) {
          toast(String(err));
        }
      };
      $('biosClearFile').onclick = clearFile;

      bindSeg('biosModeSeg', v => {
        mode = v || 'in_bios';
        renderMode();
        renderDrives();
        renderAction();
      });

      $('biosCustomName').oninput = e => {
        customName = e.target.value;
        renderMode();
        renderAction();
      };

      $('biosRefreshDrives').onclick = loadDrives;
      $('biosPrepare').onclick = () => prepareUsb(false);
      $('biosEject').onclick = ejectUsb;
    },

    async show() {
      if (!board && !detecting) {
        await detectBoard();
      } else {
        renderBoard();
      }
      renderFile();
      renderMode();
      await loadDrives();
      renderAction();
      renderReady();
    },

    drop(files) {
      if (!files || !files.length) return;
      inspectFile(files[0]);
    },

    relang() {
      renderBoard();
      renderFile();
      renderMode();
      renderDrives();
      renderAction();
      renderReady();
    },
  };
})();
