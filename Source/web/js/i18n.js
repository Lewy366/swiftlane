// Languages: English (built in), German, Polish, Italian.
// The window is written in English; every visible text, tooltip and placeholder is swapped
// for the chosen language, including text that appears later (a MutationObserver catches it).
// Text marked translate="no" (file names, maths, user input) is never touched.
const I18N = {
  de: {
    'Phone transfer': 'Handy-Übertragung', 'Tools': 'Werkzeuge', 'File converter': 'Dateikonverter', 'Image editor': 'Bildbearbeitung',
    'Video downloader': 'Video-Downloader', 'Currency': 'Währungen', 'Calculator': 'Rechner', 'Connect phone': 'Handy verbinden',
    'Settings': 'Einstellungen', 'Starting…': 'Startet…', 'Speed test': 'Speedtest', 'Ad blocker test': 'Werbeblocker-Test',
    'Send files to your phone and see what arrives from it.': 'Sende Dateien an dein Handy und sieh, was von dort ankommt.',
    'Send to phone': 'An Handy senden', 'Clear': 'Leeren', 'Drop files here': 'Dateien hier ablegen',
    'or click to browse · they appear under “From PC” on your phone': 'oder klicken zum Auswählen · sie erscheinen auf dem Handy unter „Vom PC“',
    'Received': 'Empfangen', 'Open folder': 'Ordner öffnen', 'Nothing yet': 'Noch nichts',
    'Open the Swiftlane web app on your phone,': 'Öffne die Swiftlane-Web-App auf deinem Handy,', 'or on iPhone tap Share → Send to PC.': 'oder tippe auf dem iPhone auf Teilen → An PC senden.',
    'Images, videos, audio and PDFs. Originals are never changed.': 'Bilder, Videos, Audio und PDFs. Originale bleiben unverändert.',
    'Drop files to convert': 'Dateien zum Konvertieren ablegen',
    'or click to browse · jpg, png, webp, heic, mp4, mov, mkv, mp3, wav, flac, pdf and more': 'oder klicken zum Auswählen · jpg, png, webp, heic, mp4, mov, mkv, mp3, wav, flac, pdf und mehr',
    'Files': 'Dateien', 'Convert all to': 'Alle umwandeln in', 'Quality': 'Qualität', 'High': 'Hoch', 'Balanced': 'Ausgewogen', 'Smaller file': 'Kleinere Datei',
    'Combine images into one PDF': 'Bilder zu einem PDF zusammenfügen', 'Convert': 'Konvertieren', 'Conversions': 'Konvertierungen',
    'Converted files show up here.': 'Konvertierte Dateien erscheinen hier.',
    'Crop and resize with the mouse, straighten, adjust colours, compress. Saved as “name (edited)”.': 'Mit der Maus zuschneiden und skalieren, begradigen, Farben anpassen, komprimieren.',
    'Drag to crop': 'Ziehen zum Zuschneiden', 'Crop': 'Zuschneiden', 'Drag the corner to resize': 'Ecke ziehen zum Skalieren', 'Resize': 'Größe ändern',
    'Rotate left': 'Nach links drehen', 'Rotate right': 'Nach rechts drehen', 'Flip horizontally': 'Horizontal spiegeln', 'Flip vertically': 'Vertikal spiegeln',
    'Hold to see the original': 'Gedrückt halten für das Original', 'Hold: original': 'Halten: Original', 'Exact preview': 'Genaue Vorschau',
    'Drop images here': 'Bilder hier ablegen', 'or click to browse · jpg, png, webp, heic, gif, bmp, tiff': 'oder klicken zum Auswählen · jpg, png, webp, heic, gif, bmp, tiff',
    'Crop ratio': 'Seitenverhältnis', 'Reset crop': 'Zuschnitt zurücksetzen', 'Straighten': 'Begradigen', 'Adjust': 'Anpassen', 'Reset': 'Zurücksetzen',
    'Brightness': 'Helligkeit', 'Contrast': 'Kontrast', 'Saturation': 'Sättigung', 'Sharpness': 'Schärfe', 'Save as': 'Speichern als', 'Same': 'Gleich',
    'Remove location & camera info': 'Standort- & Kameradaten entfernen', 'Save image': 'Bild speichern', 'Apply to all': 'Auf alle anwenden',
    'Saved images': 'Gespeicherte Bilder', 'Edited images show up here.': 'Bearbeitete Bilder erscheinen hier.', 'Free': 'Frei', 'Original': 'Original',
    'Save this image': 'Dieses Bild speichern', 'Saving…': 'Wird gespeichert…', '+ Add': '+ Hinzufügen', 'Only images can be edited here': 'Hier können nur Bilder bearbeitet werden',
    'estimating size…': 'Größe wird geschätzt…',
    'YouTube and many other sites. You choose where each download is saved.': 'YouTube und viele andere Seiten. Du wählst, wo jeder Download gespeichert wird.',
    'Paste a video or playlist link': 'Video- oder Playlist-Link einfügen', 'Paste': 'Einfügen', 'Look up': 'Suchen', 'Looking up…': 'Suche…',
    'Download as': 'Herunterladen als', 'Download the whole playlist': 'Ganze Playlist herunterladen', 'Download': 'Herunterladen',
    'Only download videos you have the right to save.': 'Lade nur Videos herunter, die du speichern darfst.',
    "Node.js wasn't found. YouTube may only offer some formats without it.": 'Node.js wurde nicht gefunden. Ohne Node.js bietet YouTube eventuell nur manche Formate an.',
    'Downloads': 'Downloads', 'Videos': 'Videos', 'Audio': 'Audio', 'Downloads show up here.': 'Downloads erscheinen hier.',
    'Clipboard is empty': 'Zwischenablage ist leer', 'Download started': 'Download gestartet',
    'Best quality video': 'Video in bester Qualität', '1080p video': '1080p-Video', '720p video': '720p-Video', '480p video': '480p-Video',
    'Audio only (MP3)': 'Nur Audio (MP3)', 'Audio only (M4A)': 'Nur Audio (M4A)',
    'Currency converter': 'Währungsrechner', 'European Central Bank reference rates, updated every working day.': 'Referenzkurse der Europäischen Zentralbank, jeden Werktag aktualisiert.',
    'Amount': 'Betrag', 'Swap': 'Tauschen', 'Converted': 'Umgerechnet', 'Update rates': 'Kurse aktualisieren', 'Quick view': 'Schnellansicht',
    'Scientific, modelled on the Casio fx-87DE X ClassWiz.': 'Wissenschaftlich, nach dem Vorbild des Casio fx-87DE X ClassWiz.',
    'Pair your phone once; it stays paired.': 'Verbinde dein Handy einmal; es bleibt verbunden.', 'Pair your phone': 'Handy koppeln',
    'Click to reveal': 'Klicken zum Anzeigen', "Click the code to reveal it, then scan it with your phone's camera.": 'Klicke auf den Code, um ihn anzuzeigen, und scanne ihn dann mit der Handykamera.',
    "Scan with your phone's camera to open Swiftlane.": 'Mit der Handykamera scannen, um Swiftlane zu öffnen.',
    'Pairing code': 'Kopplungscode', 'Show / hide': 'Anzeigen / verbergen', 'Copy': 'Kopieren', 'Shortcut address': 'Kurzbefehl-Adresse',
    'iPhone: “Send to PC” Shortcut': 'iPhone: Kurzbefehl „An PC senden“',
    'Adds Swiftlane to the iPhone share sheet, like AirDrop. On Android (or any phone), just use the web app from the QR code; add it to your home screen from the browser menu.':
      'Fügt Swiftlane zum Teilen-Menü des iPhones hinzu, wie AirDrop. Auf Android (oder jedem anderen Handy) nutzt du einfach die Web-App über den QR-Code; füge sie über das Browsermenü zum Startbildschirm hinzu.',
    'The full step-by-step guide is in README.md in the project folder.': 'Die vollständige Anleitung steht in README.md im Projektordner.',
    'Copy Shortcut URL': 'Kurzbefehl-URL kopieren', 'Shortcut URL copied': 'Kurzbefehl-URL kopiert', 'Pairing code copied': 'Kopplungscode kopiert',
    'Appearance': 'Darstellung', 'Accent colour': 'Akzentfarbe', 'Buttons, highlights and the open page': 'Schaltflächen, Hervorhebungen und die geöffnete Seite',
    'Background picture': 'Hintergrundbild', 'None · plain background': 'Keins · schlichter Hintergrund', 'Choose…': 'Auswählen…',
    'Remove': 'Entfernen', 'Blur': 'Unschärfe', 'Darken': 'Abdunkeln', 'Language': 'Sprache', 'The language Swiftlane is shown in': 'Die Sprache, in der Swiftlane angezeigt wird',
    'Features': 'Funktionen', 'Switched-off features disappear from the sidebar. Switching off phone transfer also stops Swiftlane listening on your network.':
      'Ausgeschaltete Funktionen verschwinden aus der Seitenleiste. Ist die Handy-Übertragung aus, lauscht Swiftlane auch nicht mehr im Netzwerk.',
    'Save received files to': 'Empfangene Dateien speichern in', 'Change': 'Ändern', 'Sort into folders': 'In Ordner sortieren',
    'Photos, Videos, Audio, Documents, Archives, Other': 'Fotos, Videos, Audio, Dokumente, Archive, Sonstiges',
    'Notifications': 'Benachrichtigungen', 'Windows notification when files arrive or a job finishes': 'Windows-Benachrichtigung, wenn Dateien ankommen oder eine Aufgabe fertig ist',
    'New pairing code': 'Neuer Kopplungscode', 'Disconnects every paired phone and Shortcut': 'Trennt alle gekoppelten Handys und Kurzbefehle',
    'Defaults': 'Standardwerte', 'Downloads are suggested in': 'Downloads werden vorgeschlagen in', 'Calculator starts in': 'Rechner startet in',
    'Angle unit for sin, cos, tan': 'Winkeleinheit für sin, cos, tan', 'General': 'Allgemein', 'Start with Windows': 'Mit Windows starten',
    'Open Swiftlane minimized when you sign in': 'Swiftlane beim Anmelden minimiert öffnen', 'Settings folder': 'Einstellungsordner',
    'Send and receive files with your phone over your network': 'Dateien mit dem Handy über dein Netzwerk senden und empfangen',
    'Images, video, audio and PDF': 'Bilder, Video, Audio und PDF', 'Crop, resize, straighten, adjust colours, compress': 'Zuschneiden, skalieren, begradigen, Farben anpassen, komprimieren',
    'YouTube and other sites': 'YouTube und andere Seiten', 'Live exchange rates': 'Aktuelle Wechselkurse', 'Scientific, ClassWiz style': 'Wissenschaftlich, im ClassWiz-Stil',
    'Phone transfer on': 'Handy-Übertragung an', 'Phone transfer off · not listening on the network': 'Handy-Übertragung aus · kein Lauschen im Netzwerk',
    'Your Swiftlane folder (Videos or Audio)': 'Dein Swiftlane-Ordner (Videos oder Audio)', 'Your own colour': 'Eigene Farbe', 'Your picture': 'Dein Bild',
    'Save folder changed': 'Speicherordner geändert', 'Couldn’t change the startup setting': 'Autostart-Einstellung konnte nicht geändert werden',
    'Make a new pairing code? Your phone and the Send to PC Shortcut will need the new code.': 'Neuen Kopplungscode erstellen? Dein Handy und der Kurzbefehl „An PC senden“ brauchen dann den neuen Code.',
    'New pairing code created · see Connect phone': 'Neuer Kopplungscode erstellt · siehe „Handy verbinden“',
    'Phone transfer off': 'Handy-Übertragung aus', 'Phone transfer unavailable': 'Handy-Übertragung nicht verfügbar',
    'Open a tool that takes files, then drop them there.': 'Öffne ein Werkzeug, das Dateien annimmt, und lege sie dort ab.',
    'Cancel': 'Abbrechen', 'Done': 'Fertig', 'Show in folder': 'Im Ordner zeigen', 'Cancelled': 'Abgebrochen', 'Failed': 'Fehlgeschlagen', 'Open': 'Öffnen',
    'Stop sharing': 'Nicht mehr teilen', 'Transfer interrupted': 'Übertragung unterbrochen',
    // calculator
    'Calculate': 'Berechnen', 'Complex': 'Komplex', 'Statistics': 'Statistik', 'Distribution': 'Verteilung', 'Equation': 'Gleichung',
    'Matrix': 'Matrix', 'Vector': 'Vektor', 'Table': 'Tabelle', 'Ratio': 'Verhältnis', 'Constants & units': 'Konstanten & Einheiten',
    'More': 'Mehr', 'Trig': 'Trig', 'Calculus': 'Analysis', 'Numbers': 'Zahlen', 'Memory': 'Speicher',
    'Exact ⇄ decimal': 'Exakt ⇄ Dezimal', 'Equation sign for SOLVE': 'Gleichheitszeichen für SOLVE', 'Division with remainder: Rem(a, b)': 'Division mit Rest: Rem(a, b)',
    'Prime factors': 'Primfaktoren', 'Store: 5→A': 'Speichern: 5→A', 'Show result in degrees, minutes, seconds': 'Ergebnis in Grad, Minuten, Sekunden zeigen',
    'Clear all variables': 'Alle Variablen löschen', 'Polar form: r∠θ': 'Polarform: r∠θ', 'Negative sign': 'Vorzeichen minus',
    'Angle unit · click to switch Deg / Rad / Gra': 'Winkeleinheit · klicken für Deg / Rad / Gra', 'Number format · click to switch Norm 1 / Norm 2 / Fix / Sci': 'Zahlenformat · klicken für Norm 1 / Norm 2 / Fix / Sci',
    'Fix: decimal places (0–9)': 'Fix: Nachkommastellen (0–9)', 'Sci: significant digits (1–10)': 'Sci: signifikante Stellen (1–10)',
    'EXACT': 'EXAKT', 'DECIMAL': 'DEZIMAL', 'Click (or S⇔D) to switch exact ⇄ decimal': 'Klicken (oder S⇔D) für exakt ⇄ dezimal', 'Complex mode': 'Komplex-Modus',
    'Engineering notation': 'Technische Schreibweise', 'Your calculations show up here.': 'Deine Rechnungen erscheinen hier.',
    'Clear all variables, Ans and memory?': 'Alle Variablen, Ans und den Speicher löschen?', 'Memory cleared': 'Speicher gelöscht',
    'Solve': 'Lösen', 'SOLVE for x · start near': 'SOLVE nach x · Start bei', 'History': 'Verlauf', 'Variables': 'Variablen',
    'Numbers are read in the selected base. Prefix d, h, b or o to mix bases (e.g. hFF + d1). Division drops the remainder, like the ClassWiz.':
      'Zahlen werden in der gewählten Basis gelesen. Präfix d, h, b oder o zum Mischen (z. B. hFF + d1). Division ohne Rest, wie beim ClassWiz.',
    'Freq': 'Häuf.', 'Regression': 'Regression', 'Estimate ŷ for x =': 'Schätze ŷ für x =', 'Estimate x̂ for y =': 'Schätze x̂ für y =',
    'Frequency column': 'Häufigkeitsspalte', '+ Row': '+ Zeile', 'Paste numbers': 'Zahlen einfügen',
    'Paste a column (or two columns) of numbers from Excel or text': 'Eine (oder zwei) Zahlenspalten aus Excel oder Text einfügen',
    'Clipboard has no numbers': 'Zwischenablage enthält keine Zahlen', 'Enter some data first': 'Bitte zuerst Daten eingeben',
    'Only numbers in the table, please': 'Bitte nur Zahlen in der Tabelle', 'Estimates': 'Schätzwerte', '1-Variable': '1 Variable', '2-Variable': '2 Variablen',
    'Data': 'Daten', 'Calculation': 'Berechnung', 'Normal PD': 'Normal-PD', 'Normal CD': 'Normal-CD', 'Lower': 'Untere', 'Upper': 'Obere',
    'Inverse Normal': 'Inverse Normal', 'Area (left tail)': 'Fläche (links)', 'Binomial PD': 'Binomial-PD', 'Binomial CD': 'Binomial-CD',
    'Poisson PD': 'Poisson-PD', 'Poisson CD': 'Poisson-CD', 'x (empty = list all)': 'x (leer = alle auflisten)',
    'Degree 2': 'Grad 2', 'Degree 3': 'Grad 3', 'Degree 4': 'Grad 4', '2 unknowns': '2 Unbekannte', '3 unknowns': '3 Unbekannte', '4 unknowns': '4 Unbekannte',
    'Simultaneous': 'Gleichungssystem', 'Polynomial': 'Polynom', 'Inequality': 'Ungleichung', 'No Solution': 'Keine Lösung', 'Infinite Solutions': 'Unendlich viele Lösungen',
    'All Real Numbers': 'Alle reellen Zahlen', 'Rows': 'Zeilen', 'Columns': 'Spalten', 'Edit vectors': 'Vektoren bearbeiten', 'Edit matrices': 'Matrizen bearbeiten',
    'Make table': 'Tabelle erstellen', 'Start': 'Start', 'End': 'Ende', 'Step': 'Schritt', 'Use': 'Einsetzen', 'Insert into Calculate': 'In „Berechnen“ einfügen',
    'Copy value': 'Wert kopieren', 'Copied': 'Kopiert', '⇄ Reverse': '⇄ Umkehren', 'Scientific constants (CODATA)': 'Naturkonstanten (CODATA)', 'Unit conversions': 'Einheiten umrechnen',
    'Base-N (32-bit)': 'Base-N (32 Bit)', 'Calculate ': 'Berechnen', 'Constant': 'Konstante',
  },
  pl: {
    'Phone transfer': 'Przesyłanie z telefonu', 'Tools': 'Narzędzia', 'File converter': 'Konwerter plików', 'Image editor': 'Edytor zdjęć',
    'Video downloader': 'Pobieranie wideo', 'Currency': 'Waluty', 'Calculator': 'Kalkulator', 'Connect phone': 'Połącz telefon',
    'Settings': 'Ustawienia', 'Starting…': 'Uruchamianie…', 'Speed test': 'Test prędkości', 'Ad blocker test': 'Test blokady reklam',
    'Send files to your phone and see what arrives from it.': 'Wysyłaj pliki na telefon i zobacz, co z niego przychodzi.',
    'Send to phone': 'Wyślij na telefon', 'Clear': 'Wyczyść', 'Drop files here': 'Upuść pliki tutaj',
    'or click to browse · they appear under “From PC” on your phone': 'lub kliknij, aby wybrać · pojawią się na telefonie w „Z komputera”',
    'Received': 'Odebrane', 'Open folder': 'Otwórz folder', 'Nothing yet': 'Jeszcze nic',
    'Open the Swiftlane web app on your phone,': 'Otwórz aplikację webową Swiftlane na telefonie,', 'or on iPhone tap Share → Send to PC.': 'lub na iPhonie stuknij Udostępnij → Wyślij na PC.',
    'Images, videos, audio and PDFs. Originals are never changed.': 'Obrazy, wideo, audio i PDF. Oryginały nigdy się nie zmieniają.',
    'Drop files to convert': 'Upuść pliki do konwersji',
    'or click to browse · jpg, png, webp, heic, mp4, mov, mkv, mp3, wav, flac, pdf and more': 'lub kliknij, aby wybrać · jpg, png, webp, heic, mp4, mov, mkv, mp3, wav, flac, pdf i inne',
    'Files': 'Pliki', 'Convert all to': 'Konwertuj wszystkie na', 'Quality': 'Jakość', 'High': 'Wysoka', 'Balanced': 'Zrównoważona', 'Smaller file': 'Mniejszy plik',
    'Combine images into one PDF': 'Połącz obrazy w jeden PDF', 'Convert': 'Konwertuj', 'Conversions': 'Konwersje',
    'Converted files show up here.': 'Tu pojawią się przekonwertowane pliki.',
    'Crop and resize with the mouse, straighten, adjust colours, compress. Saved as “name (edited)”.': 'Przycinaj i skaluj myszą, prostuj, poprawiaj kolory, kompresuj.',
    'Drag to crop': 'Przeciągnij, aby przyciąć', 'Crop': 'Przytnij', 'Drag the corner to resize': 'Przeciągnij róg, aby zmienić rozmiar', 'Resize': 'Rozmiar',
    'Rotate left': 'Obróć w lewo', 'Rotate right': 'Obróć w prawo', 'Flip horizontally': 'Odbij poziomo', 'Flip vertically': 'Odbij pionowo',
    'Hold to see the original': 'Przytrzymaj, aby zobaczyć oryginał', 'Hold: original': 'Przytrzymaj: oryginał', 'Exact preview': 'Dokładny podgląd',
    'Drop images here': 'Upuść obrazy tutaj', 'or click to browse · jpg, png, webp, heic, gif, bmp, tiff': 'lub kliknij, aby wybrać · jpg, png, webp, heic, gif, bmp, tiff',
    'Crop ratio': 'Proporcje', 'Reset crop': 'Resetuj przycięcie', 'Straighten': 'Wyprostuj', 'Adjust': 'Dostosuj', 'Reset': 'Resetuj',
    'Brightness': 'Jasność', 'Contrast': 'Kontrast', 'Saturation': 'Nasycenie', 'Sharpness': 'Ostrość', 'Save as': 'Zapisz jako', 'Same': 'Ten sam',
    'Remove location & camera info': 'Usuń lokalizację i dane aparatu', 'Save image': 'Zapisz obraz', 'Apply to all': 'Zastosuj do wszystkich',
    'Saved images': 'Zapisane obrazy', 'Edited images show up here.': 'Tu pojawią się edytowane obrazy.', 'Free': 'Dowolne', 'Original': 'Oryginalne',
    'Save this image': 'Zapisz ten obraz', 'Saving…': 'Zapisywanie…', '+ Add': '+ Dodaj', 'Only images can be edited here': 'Tu można edytować tylko obrazy',
    'estimating size…': 'szacowanie rozmiaru…',
    'YouTube and many other sites. You choose where each download is saved.': 'YouTube i wiele innych stron. Ty wybierasz, gdzie zapisać każdy plik.',
    'Paste a video or playlist link': 'Wklej link do filmu lub playlisty', 'Paste': 'Wklej', 'Look up': 'Sprawdź', 'Looking up…': 'Sprawdzanie…',
    'Download as': 'Pobierz jako', 'Download the whole playlist': 'Pobierz całą playlistę', 'Download': 'Pobierz',
    'Only download videos you have the right to save.': 'Pobieraj tylko filmy, do których masz prawo.',
    "Node.js wasn't found. YouTube may only offer some formats without it.": 'Nie znaleziono Node.js. Bez niego YouTube może oferować tylko niektóre formaty.',
    'Downloads': 'Pobrane', 'Videos': 'Wideo', 'Audio': 'Audio', 'Downloads show up here.': 'Tu pojawią się pobrane pliki.',
    'Clipboard is empty': 'Schowek jest pusty', 'Download started': 'Rozpoczęto pobieranie',
    'Best quality video': 'Wideo w najlepszej jakości', '1080p video': 'Wideo 1080p', '720p video': 'Wideo 720p', '480p video': 'Wideo 480p',
    'Audio only (MP3)': 'Tylko dźwięk (MP3)', 'Audio only (M4A)': 'Tylko dźwięk (M4A)',
    'Currency converter': 'Przelicznik walut', 'European Central Bank reference rates, updated every working day.': 'Kursy referencyjne Europejskiego Banku Centralnego, aktualizowane w dni robocze.',
    'Amount': 'Kwota', 'Swap': 'Zamień', 'Converted': 'Przeliczone', 'Update rates': 'Aktualizuj kursy', 'Quick view': 'Szybki podgląd',
    'Scientific, modelled on the Casio fx-87DE X ClassWiz.': 'Naukowy, wzorowany na Casio fx-87DE X ClassWiz.',
    'Pair your phone once; it stays paired.': 'Sparuj telefon raz; połączenie zostanie zapamiętane.', 'Pair your phone': 'Sparuj telefon',
    'Click to reveal': 'Kliknij, aby pokazać', "Click the code to reveal it, then scan it with your phone's camera.": 'Kliknij kod, aby go pokazać, a potem zeskanuj go aparatem telefonu.',
    "Scan with your phone's camera to open Swiftlane.": 'Zeskanuj aparatem telefonu, aby otworzyć Swiftlane.',
    'Pairing code': 'Kod parowania', 'Show / hide': 'Pokaż / ukryj', 'Copy': 'Kopiuj', 'Shortcut address': 'Adres skrótu',
    'iPhone: “Send to PC” Shortcut': 'iPhone: skrót „Wyślij na PC”',
    'Adds Swiftlane to the iPhone share sheet, like AirDrop. On Android (or any phone), just use the web app from the QR code; add it to your home screen from the browser menu.':
      'Dodaje Swiftlane do menu udostępniania iPhone’a, jak AirDrop. Na Androidzie (lub dowolnym telefonie) użyj aplikacji webowej z kodu QR; dodaj ją do ekranu głównego z menu przeglądarki.',
    'The full step-by-step guide is in README.md in the project folder.': 'Pełna instrukcja krok po kroku jest w pliku README.md w folderze projektu.',
    'Copy Shortcut URL': 'Kopiuj URL skrótu', 'Shortcut URL copied': 'Skopiowano URL skrótu', 'Pairing code copied': 'Skopiowano kod parowania',
    'Appearance': 'Wygląd', 'Accent colour': 'Kolor akcentu', 'Buttons, highlights and the open page': 'Przyciski, wyróżnienia i otwarta strona',
    'Background picture': 'Obraz tła', 'None · plain background': 'Brak · zwykłe tło', 'Choose…': 'Wybierz…',
    'Remove': 'Usuń', 'Blur': 'Rozmycie', 'Darken': 'Przyciemnienie', 'Language': 'Język', 'The language Swiftlane is shown in': 'Język interfejsu Swiftlane',
    'Features': 'Funkcje', 'Switched-off features disappear from the sidebar. Switching off phone transfer also stops Swiftlane listening on your network.':
      'Wyłączone funkcje znikają z paska bocznego. Wyłączenie przesyłania z telefonu sprawia też, że Swiftlane przestaje nasłuchiwać w sieci.',
    'Save received files to': 'Zapisuj odebrane pliki w', 'Change': 'Zmień', 'Sort into folders': 'Sortuj do folderów',
    'Photos, Videos, Audio, Documents, Archives, Other': 'Zdjęcia, Wideo, Audio, Dokumenty, Archiwa, Inne',
    'Notifications': 'Powiadomienia', 'Windows notification when files arrive or a job finishes': 'Powiadomienie Windows, gdy przyjdą pliki lub zadanie się zakończy',
    'New pairing code': 'Nowy kod parowania', 'Disconnects every paired phone and Shortcut': 'Rozłącza wszystkie sparowane telefony i skróty',
    'Defaults': 'Domyślne', 'Downloads are suggested in': 'Pobrane pliki proponowane w', 'Calculator starts in': 'Kalkulator startuje w',
    'Angle unit for sin, cos, tan': 'Jednostka kąta dla sin, cos, tan', 'General': 'Ogólne', 'Start with Windows': 'Uruchamiaj z Windows',
    'Open Swiftlane minimized when you sign in': 'Otwieraj Swiftlane zminimalizowany po zalogowaniu', 'Settings folder': 'Folder ustawień',
    'Send and receive files with your phone over your network': 'Wysyłaj i odbieraj pliki z telefonu przez swoją sieć',
    'Images, video, audio and PDF': 'Obrazy, wideo, audio i PDF', 'Crop, resize, straighten, adjust colours, compress': 'Przycinanie, skalowanie, prostowanie, kolory, kompresja',
    'YouTube and other sites': 'YouTube i inne strony', 'Live exchange rates': 'Aktualne kursy walut', 'Scientific, ClassWiz style': 'Naukowy, w stylu ClassWiz',
    'Phone transfer on': 'Przesyłanie z telefonu włączone', 'Phone transfer off · not listening on the network': 'Przesyłanie wyłączone · brak nasłuchu w sieci',
    'Your Swiftlane folder (Videos or Audio)': 'Twój folder Swiftlane (Wideo lub Audio)', 'Your own colour': 'Własny kolor', 'Your picture': 'Twój obraz',
    'Save folder changed': 'Zmieniono folder zapisu', 'Couldn’t change the startup setting': 'Nie udało się zmienić ustawienia autostartu',
    'Make a new pairing code? Your phone and the Send to PC Shortcut will need the new code.': 'Utworzyć nowy kod parowania? Telefon i skrót „Wyślij na PC” będą potrzebować nowego kodu.',
    'New pairing code created · see Connect phone': 'Utworzono nowy kod · zobacz „Połącz telefon”',
    'Phone transfer off': 'Przesyłanie wyłączone', 'Phone transfer unavailable': 'Przesyłanie niedostępne',
    'Open a tool that takes files, then drop them there.': 'Otwórz narzędzie przyjmujące pliki i upuść je tam.',
    'Cancel': 'Anuluj', 'Done': 'Gotowe', 'Show in folder': 'Pokaż w folderze', 'Cancelled': 'Anulowano', 'Failed': 'Niepowodzenie', 'Open': 'Otwórz',
    'Stop sharing': 'Przestań udostępniać', 'Transfer interrupted': 'Przesyłanie przerwane',
    'Calculate': 'Obliczenia', 'Complex': 'Zespolone', 'Statistics': 'Statystyka', 'Distribution': 'Rozkład', 'Equation': 'Równania',
    'Matrix': 'Macierze', 'Vector': 'Wektory', 'Table': 'Tabela', 'Ratio': 'Proporcja', 'Constants & units': 'Stałe i jednostki',
    'More': 'Więcej', 'Trig': 'Tryg.', 'Calculus': 'Analiza', 'Numbers': 'Liczby', 'Memory': 'Pamięć',
    'Exact ⇄ decimal': 'Dokładnie ⇄ dziesiętnie', 'Equation sign for SOLVE': 'Znak równości dla SOLVE', 'Division with remainder: Rem(a, b)': 'Dzielenie z resztą: Rem(a, b)',
    'Prime factors': 'Czynniki pierwsze', 'Store: 5→A': 'Zapisz: 5→A', 'Show result in degrees, minutes, seconds': 'Pokaż wynik w stopniach, minutach, sekundach',
    'Clear all variables': 'Wyczyść wszystkie zmienne', 'Polar form: r∠θ': 'Postać biegunowa: r∠θ', 'Negative sign': 'Znak minus',
    'Angle unit · click to switch Deg / Rad / Gra': 'Jednostka kąta · kliknij: Deg / Rad / Gra', 'Number format · click to switch Norm 1 / Norm 2 / Fix / Sci': 'Format liczb · kliknij: Norm 1 / Norm 2 / Fix / Sci',
    'Fix: decimal places (0–9)': 'Fix: miejsca po przecinku (0–9)', 'Sci: significant digits (1–10)': 'Sci: cyfry znaczące (1–10)',
    'EXACT': 'DOKŁ.', 'DECIMAL': 'DZIES.', 'Click (or S⇔D) to switch exact ⇄ decimal': 'Kliknij (lub S⇔D): dokładnie ⇄ dziesiętnie', 'Complex mode': 'Tryb zespolony',
    'Engineering notation': 'Notacja inżynierska', 'Your calculations show up here.': 'Tu pojawią się twoje obliczenia.',
    'Clear all variables, Ans and memory?': 'Wyczyścić wszystkie zmienne, Ans i pamięć?', 'Memory cleared': 'Pamięć wyczyszczona',
    'Solve': 'Rozwiąż', 'SOLVE for x · start near': 'SOLVE dla x · start w pobliżu', 'History': 'Historia', 'Variables': 'Zmienne',
    'Numbers are read in the selected base. Prefix d, h, b or o to mix bases (e.g. hFF + d1). Division drops the remainder, like the ClassWiz.':
      'Liczby są czytane w wybranym systemie. Przedrostki d, h, b, o mieszają systemy (np. hFF + d1). Dzielenie pomija resztę, jak w ClassWiz.',
    'Freq': 'Częst.', 'Regression': 'Regresja', 'Estimate ŷ for x =': 'Oszacuj ŷ dla x =', 'Estimate x̂ for y =': 'Oszacuj x̂ dla y =',
    'Frequency column': 'Kolumna częstości', '+ Row': '+ Wiersz', 'Paste numbers': 'Wklej liczby',
    'Paste a column (or two columns) of numbers from Excel or text': 'Wklej kolumnę (lub dwie) liczb z Excela lub tekstu',
    'Clipboard has no numbers': 'W schowku nie ma liczb', 'Enter some data first': 'Najpierw wprowadź dane',
    'Only numbers in the table, please': 'W tabeli tylko liczby', 'Estimates': 'Oszacowania', '1-Variable': '1 zmienna', '2-Variable': '2 zmienne',
    'Data': 'Dane', 'Calculation': 'Obliczenia', 'Lower': 'Dolna', 'Upper': 'Górna', 'Inverse Normal': 'Odwrotny normalny', 'Area (left tail)': 'Pole (lewy ogon)',
    'x (empty = list all)': 'x (puste = wszystkie)',
    'Degree 2': 'Stopień 2', 'Degree 3': 'Stopień 3', 'Degree 4': 'Stopień 4', '2 unknowns': '2 niewiadome', '3 unknowns': '3 niewiadome', '4 unknowns': '4 niewiadome',
    'Simultaneous': 'Układ równań', 'Polynomial': 'Wielomian', 'Inequality': 'Nierówność', 'No Solution': 'Brak rozwiązania', 'Infinite Solutions': 'Nieskończenie wiele rozwiązań',
    'All Real Numbers': 'Wszystkie liczby rzeczywiste', 'Rows': 'Wiersze', 'Columns': 'Kolumny', 'Edit vectors': 'Edytuj wektory', 'Edit matrices': 'Edytuj macierze',
    'Make table': 'Utwórz tabelę', 'Start': 'Początek', 'End': 'Koniec', 'Step': 'Krok', 'Use': 'Wstaw', 'Insert into Calculate': 'Wstaw do obliczeń',
    'Copy value': 'Kopiuj wartość', 'Copied': 'Skopiowano', '⇄ Reverse': '⇄ Odwróć', 'Scientific constants (CODATA)': 'Stałe fizyczne (CODATA)', 'Unit conversions': 'Przeliczanie jednostek',
    'Base-N (32-bit)': 'Base-N (32 bity)',
  },
  it: {
    'Phone transfer': 'Trasferimento telefono', 'Tools': 'Strumenti', 'File converter': 'Convertitore file', 'Image editor': 'Editor immagini',
    'Video downloader': 'Download video', 'Currency': 'Valute', 'Calculator': 'Calcolatrice', 'Connect phone': 'Collega telefono',
    'Settings': 'Impostazioni', 'Starting…': 'Avvio…', 'Speed test': 'Test velocità', 'Ad blocker test': 'Test blocco pubblicità',
    'Send files to your phone and see what arrives from it.': 'Invia file al telefono e guarda cosa arriva da lì.',
    'Send to phone': 'Invia al telefono', 'Clear': 'Svuota', 'Drop files here': 'Trascina qui i file',
    'or click to browse · they appear under “From PC” on your phone': 'o fai clic per sceglierli · compaiono sul telefono in “Dal PC”',
    'Received': 'Ricevuti', 'Open folder': 'Apri cartella', 'Nothing yet': 'Ancora niente',
    'Open the Swiftlane web app on your phone,': 'Apri la web app Swiftlane sul telefono,', 'or on iPhone tap Share → Send to PC.': 'oppure su iPhone tocca Condividi → Invia al PC.',
    'Images, videos, audio and PDFs. Originals are never changed.': 'Immagini, video, audio e PDF. Gli originali non vengono mai modificati.',
    'Drop files to convert': 'Trascina qui i file da convertire',
    'or click to browse · jpg, png, webp, heic, mp4, mov, mkv, mp3, wav, flac, pdf and more': 'o fai clic per sceglierli · jpg, png, webp, heic, mp4, mov, mkv, mp3, wav, flac, pdf e altro',
    'Files': 'File', 'Convert all to': 'Converti tutti in', 'Quality': 'Qualità', 'High': 'Alta', 'Balanced': 'Bilanciata', 'Smaller file': 'File più piccolo',
    'Combine images into one PDF': 'Unisci le immagini in un PDF', 'Convert': 'Converti', 'Conversions': 'Conversioni',
    'Converted files show up here.': 'Qui compaiono i file convertiti.',
    'Crop and resize with the mouse, straighten, adjust colours, compress. Saved as “name (edited)”.': 'Ritaglia e ridimensiona con il mouse, raddrizza, regola i colori, comprimi.',
    'Drag to crop': 'Trascina per ritagliare', 'Crop': 'Ritaglia', 'Drag the corner to resize': 'Trascina l’angolo per ridimensionare', 'Resize': 'Ridimensiona',
    'Rotate left': 'Ruota a sinistra', 'Rotate right': 'Ruota a destra', 'Flip horizontally': 'Rifletti orizzontalmente', 'Flip vertically': 'Rifletti verticalmente',
    'Hold to see the original': 'Tieni premuto per vedere l’originale', 'Hold: original': 'Tieni: originale', 'Exact preview': 'Anteprima esatta',
    'Drop images here': 'Trascina qui le immagini', 'or click to browse · jpg, png, webp, heic, gif, bmp, tiff': 'o fai clic per sceglierle · jpg, png, webp, heic, gif, bmp, tiff',
    'Crop ratio': 'Proporzioni', 'Reset crop': 'Annulla ritaglio', 'Straighten': 'Raddrizza', 'Adjust': 'Regola', 'Reset': 'Ripristina',
    'Brightness': 'Luminosità', 'Contrast': 'Contrasto', 'Saturation': 'Saturazione', 'Sharpness': 'Nitidezza', 'Save as': 'Salva come', 'Same': 'Uguale',
    'Remove location & camera info': 'Rimuovi posizione e dati fotocamera', 'Save image': 'Salva immagine', 'Apply to all': 'Applica a tutte',
    'Saved images': 'Immagini salvate', 'Edited images show up here.': 'Qui compaiono le immagini modificate.', 'Free': 'Libero', 'Original': 'Originale',
    'Save this image': 'Salva questa immagine', 'Saving…': 'Salvataggio…', '+ Add': '+ Aggiungi', 'Only images can be edited here': 'Qui si possono modificare solo immagini',
    'estimating size…': 'stima delle dimensioni…',
    'YouTube and many other sites. You choose where each download is saved.': 'YouTube e molti altri siti. Scegli tu dove salvare ogni download.',
    'Paste a video or playlist link': 'Incolla il link di un video o di una playlist', 'Paste': 'Incolla', 'Look up': 'Cerca', 'Looking up…': 'Ricerca…',
    'Download as': 'Scarica come', 'Download the whole playlist': 'Scarica tutta la playlist', 'Download': 'Scarica',
    'Only download videos you have the right to save.': 'Scarica solo video che hai il diritto di salvare.',
    "Node.js wasn't found. YouTube may only offer some formats without it.": 'Node.js non trovato. Senza, YouTube potrebbe offrire solo alcuni formati.',
    'Downloads': 'Download', 'Videos': 'Video', 'Audio': 'Audio', 'Downloads show up here.': 'Qui compaiono i download.',
    'Clipboard is empty': 'Gli appunti sono vuoti', 'Download started': 'Download avviato',
    'Best quality video': 'Video alla massima qualità', '1080p video': 'Video 1080p', '720p video': 'Video 720p', '480p video': 'Video 480p',
    'Audio only (MP3)': 'Solo audio (MP3)', 'Audio only (M4A)': 'Solo audio (M4A)',
    'Currency converter': 'Convertitore di valute', 'European Central Bank reference rates, updated every working day.': 'Tassi di riferimento della Banca Centrale Europea, aggiornati ogni giorno lavorativo.',
    'Amount': 'Importo', 'Swap': 'Inverti', 'Converted': 'Convertito', 'Update rates': 'Aggiorna tassi', 'Quick view': 'Vista rapida',
    'Scientific, modelled on the Casio fx-87DE X ClassWiz.': 'Scientifica, ispirata alla Casio fx-87DE X ClassWiz.',
    'Pair your phone once; it stays paired.': 'Associa il telefono una volta; resta associato.', 'Pair your phone': 'Associa il telefono',
    'Click to reveal': 'Clic per mostrare', "Click the code to reveal it, then scan it with your phone's camera.": 'Fai clic sul codice per mostrarlo, poi inquadralo con la fotocamera del telefono.',
    "Scan with your phone's camera to open Swiftlane.": 'Inquadra con la fotocamera del telefono per aprire Swiftlane.',
    'Pairing code': 'Codice di associazione', 'Show / hide': 'Mostra / nascondi', 'Copy': 'Copia', 'Shortcut address': 'Indirizzo per Comandi rapidi',
    'iPhone: “Send to PC” Shortcut': 'iPhone: comando rapido “Invia al PC”',
    'Adds Swiftlane to the iPhone share sheet, like AirDrop. On Android (or any phone), just use the web app from the QR code; add it to your home screen from the browser menu.':
      'Aggiunge Swiftlane al menu Condividi dell’iPhone, come AirDrop. Su Android (o qualsiasi telefono) usa la web app dal codice QR; aggiungila alla schermata Home dal menu del browser.',
    'The full step-by-step guide is in README.md in the project folder.': 'La guida completa passo passo è in README.md nella cartella del progetto.',
    'Copy Shortcut URL': 'Copia URL del comando', 'Shortcut URL copied': 'URL del comando copiato', 'Pairing code copied': 'Codice copiato',
    'Appearance': 'Aspetto', 'Accent colour': 'Colore principale', 'Buttons, highlights and the open page': 'Pulsanti, evidenziazioni e la pagina aperta',
    'Background picture': 'Immagine di sfondo', 'None · plain background': 'Nessuna · sfondo semplice', 'Choose…': 'Scegli…',
    'Remove': 'Rimuovi', 'Blur': 'Sfocatura', 'Darken': 'Scurisci', 'Language': 'Lingua', 'The language Swiftlane is shown in': 'La lingua di Swiftlane',
    'Features': 'Funzioni', 'Switched-off features disappear from the sidebar. Switching off phone transfer also stops Swiftlane listening on your network.':
      'Le funzioni disattivate spariscono dalla barra laterale. Disattivando il trasferimento telefono, Swiftlane smette anche di ascoltare in rete.',
    'Save received files to': 'Salva i file ricevuti in', 'Change': 'Cambia', 'Sort into folders': 'Ordina in cartelle',
    'Photos, Videos, Audio, Documents, Archives, Other': 'Foto, Video, Audio, Documenti, Archivi, Altro',
    'Notifications': 'Notifiche', 'Windows notification when files arrive or a job finishes': 'Notifica di Windows quando arrivano file o finisce un’operazione',
    'New pairing code': 'Nuovo codice', 'Disconnects every paired phone and Shortcut': 'Scollega tutti i telefoni e i comandi rapidi associati',
    'Defaults': 'Predefiniti', 'Downloads are suggested in': 'I download vengono proposti in', 'Calculator starts in': 'La calcolatrice parte in',
    'Angle unit for sin, cos, tan': 'Unità angolare per sin, cos, tan', 'General': 'Generale', 'Start with Windows': 'Avvia con Windows',
    'Open Swiftlane minimized when you sign in': 'Apri Swiftlane ridotto a icona all’accesso', 'Settings folder': 'Cartella impostazioni',
    'Send and receive files with your phone over your network': 'Invia e ricevi file con il telefono attraverso la tua rete',
    'Images, video, audio and PDF': 'Immagini, video, audio e PDF', 'Crop, resize, straighten, adjust colours, compress': 'Ritaglio, ridimensionamento, raddrizzamento, colori, compressione',
    'YouTube and other sites': 'YouTube e altri siti', 'Live exchange rates': 'Tassi di cambio aggiornati', 'Scientific, ClassWiz style': 'Scientifica, stile ClassWiz',
    'Phone transfer on': 'Trasferimento attivo', 'Phone transfer off · not listening on the network': 'Trasferimento disattivato · nessun ascolto in rete',
    'Your Swiftlane folder (Videos or Audio)': 'La tua cartella Swiftlane (Video o Audio)', 'Your own colour': 'Colore personalizzato', 'Your picture': 'La tua immagine',
    'Save folder changed': 'Cartella di salvataggio cambiata', 'Couldn’t change the startup setting': 'Impossibile cambiare l’avvio automatico',
    'Make a new pairing code? Your phone and the Send to PC Shortcut will need the new code.': 'Creare un nuovo codice? Il telefono e il comando “Invia al PC” avranno bisogno del nuovo codice.',
    'New pairing code created · see Connect phone': 'Nuovo codice creato · vedi “Collega telefono”',
    'Phone transfer off': 'Trasferimento disattivato', 'Phone transfer unavailable': 'Trasferimento non disponibile',
    'Open a tool that takes files, then drop them there.': 'Apri uno strumento che accetta file e trascinali lì.',
    'Cancel': 'Annulla', 'Done': 'Fatto', 'Show in folder': 'Mostra nella cartella', 'Cancelled': 'Annullato', 'Failed': 'Non riuscito', 'Open': 'Apri',
    'Stop sharing': 'Smetti di condividere', 'Transfer interrupted': 'Trasferimento interrotto',
    'Calculate': 'Calcolo', 'Complex': 'Complessi', 'Statistics': 'Statistica', 'Distribution': 'Distribuzione', 'Equation': 'Equazioni',
    'Matrix': 'Matrici', 'Vector': 'Vettori', 'Table': 'Tabella', 'Ratio': 'Proporzione', 'Constants & units': 'Costanti e unità',
    'More': 'Altro', 'Trig': 'Trig', 'Calculus': 'Analisi', 'Numbers': 'Numeri', 'Memory': 'Memoria',
    'Exact ⇄ decimal': 'Esatto ⇄ decimale', 'Equation sign for SOLVE': 'Segno di uguale per SOLVE', 'Division with remainder: Rem(a, b)': 'Divisione con resto: Rem(a, b)',
    'Prime factors': 'Fattori primi', 'Store: 5→A': 'Memorizza: 5→A', 'Show result in degrees, minutes, seconds': 'Mostra il risultato in gradi, minuti, secondi',
    'Clear all variables': 'Cancella tutte le variabili', 'Polar form: r∠θ': 'Forma polare: r∠θ', 'Negative sign': 'Segno meno',
    'Angle unit · click to switch Deg / Rad / Gra': 'Unità angolare · clic: Deg / Rad / Gra', 'Number format · click to switch Norm 1 / Norm 2 / Fix / Sci': 'Formato numeri · clic: Norm 1 / Norm 2 / Fix / Sci',
    'Fix: decimal places (0–9)': 'Fix: cifre decimali (0–9)', 'Sci: significant digits (1–10)': 'Sci: cifre significative (1–10)',
    'EXACT': 'ESATTO', 'DECIMAL': 'DECIMALE', 'Click (or S⇔D) to switch exact ⇄ decimal': 'Clic (o S⇔D): esatto ⇄ decimale', 'Complex mode': 'Modalità complessi',
    'Engineering notation': 'Notazione ingegneristica', 'Your calculations show up here.': 'Qui compaiono i tuoi calcoli.',
    'Clear all variables, Ans and memory?': 'Cancellare tutte le variabili, Ans e la memoria?', 'Memory cleared': 'Memoria cancellata',
    'Solve': 'Risolvi', 'SOLVE for x · start near': 'SOLVE per x · parti da', 'History': 'Cronologia', 'Variables': 'Variabili',
    'Numbers are read in the selected base. Prefix d, h, b or o to mix bases (e.g. hFF + d1). Division drops the remainder, like the ClassWiz.':
      'I numeri sono letti nella base scelta. Prefissi d, h, b, o per mescolare basi (es. hFF + d1). La divisione scarta il resto, come la ClassWiz.',
    'Freq': 'Freq.', 'Regression': 'Regressione', 'Estimate ŷ for x =': 'Stima ŷ per x =', 'Estimate x̂ for y =': 'Stima x̂ per y =',
    'Frequency column': 'Colonna frequenze', '+ Row': '+ Riga', 'Paste numbers': 'Incolla numeri',
    'Paste a column (or two columns) of numbers from Excel or text': 'Incolla una colonna (o due) di numeri da Excel o testo',
    'Clipboard has no numbers': 'Negli appunti non ci sono numeri', 'Enter some data first': 'Inserisci prima dei dati',
    'Only numbers in the table, please': 'Solo numeri nella tabella', 'Estimates': 'Stime', '1-Variable': '1 variabile', '2-Variable': '2 variabili',
    'Data': 'Dati', 'Calculation': 'Calcolo', 'Lower': 'Inferiore', 'Upper': 'Superiore', 'Inverse Normal': 'Normale inversa', 'Area (left tail)': 'Area (coda sinistra)',
    'x (empty = list all)': 'x (vuoto = elenca tutti)',
    'Degree 2': 'Grado 2', 'Degree 3': 'Grado 3', 'Degree 4': 'Grado 4', '2 unknowns': '2 incognite', '3 unknowns': '3 incognite', '4 unknowns': '4 incognite',
    'Simultaneous': 'Sistema', 'Polynomial': 'Polinomio', 'Inequality': 'Disequazione', 'No Solution': 'Nessuna soluzione', 'Infinite Solutions': 'Infinite soluzioni',
    'All Real Numbers': 'Tutti i numeri reali', 'Rows': 'Righe', 'Columns': 'Colonne', 'Edit vectors': 'Modifica vettori', 'Edit matrices': 'Modifica matrici',
    'Make table': 'Crea tabella', 'Start': 'Inizio', 'End': 'Fine', 'Step': 'Passo', 'Use': 'Usa', 'Insert into Calculate': 'Inserisci in Calcolo',
    'Copy value': 'Copia valore', 'Copied': 'Copiato', '⇄ Reverse': '⇄ Inverti', 'Scientific constants (CODATA)': 'Costanti scientifiche (CODATA)', 'Unit conversions': 'Conversioni di unità',
    'Base-N (32-bit)': 'Base-N (32 bit)',
  },
};

// Text with numbers or names in it.
const I18N_PATTERNS = {
  de: [[/^Convert (\d+) files$/, '$1 Dateien konvertieren'], [/^Done · (\d+) files$/, 'Fertig · $1 Dateien'], [/^Apply to all (\d+) images$/, 'Auf alle $1 Bilder anwenden'],
       [/^Apply to (\d+) images?$/, 'Auf $1 Bild(er) anwenden'], [/^Saving (\d+) images…$/, '$1 Bilder werden gespeichert…'], [/^Edit (\d+) images?$/, '$1 Bild(er) bearbeiten'],
       [/^(\d+) images → one PDF$/, '$1 Bilder → ein PDF'], [/^Download · (.+)$/, 'Download · $1'], [/^(\d+) file\(s\) ready for your phone$/, '$1 Datei(en) bereit für dein Handy'],
       [/^(\d+) file\(s\) skipped: that type can’t be converted$/, '$1 Datei(en) übersprungen: dieser Typ lässt sich nicht konvertieren'], [/^Ready · (.+)$/, 'Bereit · $1'],
       [/^Receiving · (.+) of (.+)$/, 'Empfange · $1 von $2'], [/^“(.+)” saved on your phone$/, '„$1“ auf dem Handy gespeichert'], [/^Rates of (.+)$/, 'Kurse vom $1'],
       [/^(.+) in other currencies$/, '$1 in anderen Währungen'], [/^Save (Mat|Vct)(.)$/, '$1$2 speichern'], [/^Vertex \((minimum|maximum)\)$/, (m, k) => k === 'minimum' ? 'Scheitelpunkt (Minimum)' : 'Scheitelpunkt (Maximum)'],
       [/^Stored in (.)$/, 'Gespeichert in $1'], [/^ · saved on phone( ×\d+)?$/, ' · auf dem Handy gespeichert$1'], [/^(\d+) of (\d+) · (.+)$/, '$1 von $2 · $3'],
       [/^Starting…$/, 'Startet…'], [/^Merging video and audio…$/, 'Video und Ton werden zusammengeführt…'], [/^Making audio file…$/, 'Audiodatei wird erstellt…'], [/^Processing…$/, 'Wird verarbeitet…']],
  pl: [[/^Convert (\d+) files$/, 'Konwertuj pliki: $1'], [/^Done · (\d+) files$/, 'Gotowe · plików: $1'], [/^Apply to all (\d+) images$/, 'Zastosuj do wszystkich ($1)'],
       [/^Apply to (\d+) images?$/, 'Zastosuj do obrazów: $1'], [/^Saving (\d+) images…$/, 'Zapisywanie obrazów: $1…'], [/^Edit (\d+) images?$/, 'Edycja obrazów: $1'],
       [/^(\d+) images → one PDF$/, 'Obrazy ($1) → jeden PDF'], [/^Download · (.+)$/, 'Pobieranie · $1'], [/^(\d+) file\(s\) ready for your phone$/, 'Pliki gotowe dla telefonu: $1'],
       [/^(\d+) file\(s\) skipped: that type can’t be converted$/, 'Pominięto pliki: $1 (tego typu nie da się konwertować)'], [/^Ready · (.+)$/, 'Gotowe · $1'],
       [/^Receiving · (.+) of (.+)$/, 'Odbieranie · $1 z $2'], [/^“(.+)” saved on your phone$/, '„$1” zapisano na telefonie'], [/^Rates of (.+)$/, 'Kursy z $1'],
       [/^(.+) in other currencies$/, '$1 w innych walutach'], [/^Save (Mat|Vct)(.)$/, 'Zapisz $1$2'], [/^Vertex \((minimum|maximum)\)$/, (m, k) => k === 'minimum' ? 'Wierzchołek (minimum)' : 'Wierzchołek (maksimum)'],
       [/^Stored in (.)$/, 'Zapisano w $1'], [/^ · saved on phone( ×\d+)?$/, ' · zapisano na telefonie$1'], [/^(\d+) of (\d+) · (.+)$/, '$1 z $2 · $3'],
       [/^Merging video and audio…$/, 'Łączenie obrazu i dźwięku…'], [/^Making audio file…$/, 'Tworzenie pliku audio…'], [/^Processing…$/, 'Przetwarzanie…']],
  it: [[/^Convert (\d+) files$/, 'Converti $1 file'], [/^Done · (\d+) files$/, 'Fatto · $1 file'], [/^Apply to all (\d+) images$/, 'Applica a tutte le $1 immagini'],
       [/^Apply to (\d+) images?$/, 'Applica a $1 immagini'], [/^Saving (\d+) images…$/, 'Salvataggio di $1 immagini…'], [/^Edit (\d+) images?$/, 'Modifica $1 immagini'],
       [/^(\d+) images → one PDF$/, '$1 immagini → un PDF'], [/^Download · (.+)$/, 'Download · $1'], [/^(\d+) file\(s\) ready for your phone$/, '$1 file pronti per il telefono'],
       [/^(\d+) file\(s\) skipped: that type can’t be converted$/, '$1 file saltati: tipo non convertibile'], [/^Ready · (.+)$/, 'Pronto · $1'],
       [/^Receiving · (.+) of (.+)$/, 'Ricezione · $1 di $2'], [/^“(.+)” saved on your phone$/, '“$1” salvato sul telefono'], [/^Rates of (.+)$/, 'Tassi del $1'],
       [/^(.+) in other currencies$/, '$1 in altre valute'], [/^Save (Mat|Vct)(.)$/, 'Salva $1$2'], [/^Vertex \((minimum|maximum)\)$/, (m, k) => k === 'minimum' ? 'Vertice (minimo)' : 'Vertice (massimo)'],
       [/^Stored in (.)$/, 'Memorizzato in $1'], [/^ · saved on phone( ×\d+)?$/, ' · salvato sul telefono$1'], [/^(\d+) of (\d+) · (.+)$/, '$1 di $2 · $3'],
       [/^Merging video and audio…$/, 'Unione di video e audio…'], [/^Making audio file…$/, 'Creazione del file audio…'], [/^Processing…$/, 'Elaborazione…']],
};

let LANG = 'en';
const SKIP = 'math, code, script, style, svg, input, textarea, select option, [translate="no"], .q, .expr, .answer, .mono, .mat-grid';

function t(s) {
  if (LANG === 'en' || s == null) return s;
  const dict = I18N[LANG] || {};
  const core = String(s).trim();
  if (dict[core] !== undefined) return String(s).replace(core, dict[core]);
  const dl = core.match(/^Download · (.+)$/);  // job titles: "Download · 720p video"
  if (dl) return String(s).replace(core, ({ pl: 'Pobieranie', it: 'Download' }[LANG] || 'Download') + ' · ' + t(dl[1]));
  for (const [re, rep] of I18N_PATTERNS[LANG] || []) {
    if (re.test(core)) return String(s).replace(core, core.replace(re, rep));
  }
  return s;
}

function translateNode(node) {
  if (node.nodeType === Node.TEXT_NODE) {
    const parent = node.parentElement;
    if (!parent || parent.closest(SKIP)) return;
    if (node.__tr !== undefined && node.nodeValue === node.__tr) {          // ours: retranslate from the English
      const out = t(node.__en); if (out !== node.nodeValue) { node.__tr = out; node.nodeValue = out; } return;
    }
    node.__en = node.nodeValue;                                            // new English text
    const out = t(node.__en);
    node.__tr = out;
    if (out !== node.nodeValue) node.nodeValue = out;
    return;
  }
  if (node.nodeType !== Node.ELEMENT_NODE) return;
  const skipped = node.matches(SKIP);  // inputs are skipped for their content, but their hint text is app text
  if (skipped && !(node.matches('input, textarea') && !node.closest('[translate="no"]'))) return;
  for (const attr of ['placeholder', 'title']) {
    if (!node.hasAttribute(attr)) continue;
    const key = 'en' + attr;
    const current = node.getAttribute(attr);
    if (node.dataset[key] === undefined || (node.__attrTr || {})[attr] !== current) node.dataset[key] = current;
    const out = t(node.dataset[key]);
    node.__attrTr = { ...(node.__attrTr || {}), [attr]: out };
    if (out !== current) node.setAttribute(attr, out);
  }
  if (skipped) return;
  for (const child of node.childNodes) translateNode(child);
}

const i18nObserver = new MutationObserver(records => {
  if (LANG === 'en') return;
  for (const r of records) {
    if (r.type === 'characterData') translateNode(r.target);
    else if (r.type === 'attributes') translateNode(r.target);
    else r.addedNodes.forEach(translateNode);
  }
});

function setLanguage(lang) {
  const prev = LANG;
  LANG = I18N[lang] ? lang : 'en';
  document.documentElement.lang = LANG;
  if (prev !== LANG) {
    // back to English first, then into the new language
    const restore = n => {
      if (n.nodeType === Node.TEXT_NODE) { if (n.__en !== undefined && n.nodeValue === n.__tr) { n.nodeValue = n.__en; n.__tr = n.__en; } return; }
      if (n.nodeType !== Node.ELEMENT_NODE) return;
      for (const attr of ['placeholder', 'title']) if (n.dataset && n.dataset['en' + attr] !== undefined) n.setAttribute(attr, n.dataset['en' + attr]);
      n.childNodes.forEach(restore);
    };
    restore(document.body);
  }
  translateNode(document.body);
  i18nObserver.disconnect();
  if (LANG !== 'en') i18nObserver.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ['placeholder', 'title'] });
  if (typeof App !== 'undefined' && App.pages) for (const p of Object.values(App.pages)) if (p.relang) p.relang();
}
