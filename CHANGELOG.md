# Änderungen

Jede Version zählt die letzte Stelle hoch. Neueste Version oben.

## 1.0.022 · 2026-11-14 10:48

- Export: „Brennen“ ist keine eigene Ausgabe mehr, sondern die Option „Danach brennen“ beim ISO-Abbild.
- Export als Einzeldateien: Bei VOB, MPEG-2 und MKV mit mehreren Titeln speichert „Jeden Titel als eigene Datei speichern“ jeden gewählten Titel in eine eigene Datei im gewählten Ordner. Dateiname ist der Name des Titels in der Liste darunter; gleiche Namen werden durchnummeriert. Die Tiefenanalyse läuft nur einmal, „Zur Warteschlange“ legt je Datei einen Eintrag an. Bei Abbruch bleiben schon fertige Dateien erhalten.
- Die Namen in der Titelliste des Export-Dialogs lassen sich ändern. Beim Neu-Zusammenstellen bleibt der Name am Eintrag, wird im Projekt gespeichert und erscheint auch in der Liste „Neue DVD“.
- Neue Einstellungen unter „Vorschau“: „Vorschaubilder der Einträge dauerhaft abspielen“ lässt die kleinen Anfangs- und Endbilder in „Neue DVD“ ohne Maus als Schleife laufen, „Länge der Schleife“ stellt die Sekunden ein (gilt auch beim Überfahren mit der Maus). Liegt die Quelle auf einer DVD im Laufwerk, laufen die Schleifen nur beim Überfahren mit der Maus.

## 1.0.021 · 2026-11-13 12:04

- „Titel bei Klicken automatisch abspielen“ gilt jetzt auch für die Liste „Neue DVD“: Ein Klick auf einen hinzugefügten, geschnittenen oder zerlegten Eintrag spielt ihn gleich ab dem Schnittanfang ab. Strg+Klick und Umschalt+Klick (Mehrfachauswahl) starten nichts.

## 1.0.020 · 2026-11-13 11:48

- Neue Einstellung „Titel bei Klicken automatisch abspielen“ (Einstellungen → Vorschau): Ein Klick auf einen Titel in der Liste links lädt ihn und spielt ihn sofort ab, mit Ton. Gilt für die Titelliste bei „Neu zusammenstellen“ und bei „Ganze DVD“. Standardmäßig aus.

## 1.0.019 · 2026-11-13 10:15

- Startseite: „Mehrere DVDs nacheinander“ heißt jetzt „Warteschlange abarbeiten“.
- Exportierte Dateien behalten den Namen der Quelle und bekommen nur „_converted“ angehängt: Aus Test.vob wird Test_converted.vob (bisher „Test (klein).vob“). Punkte, Klammern und andere in Windows erlaubte Zeichen bleiben im Namen erhalten. Gilt auch für ISO, DVD-Ordner, MPEG, MKV und die Warteschlange.

## 1.0.018 · 2026-11-12 09:40

- Neue Ausgabe „MKV“ im Export: Bild und eine Tonspur wie bei MPEG-2, ohne Neukodierung, aber als Matroska-Datei mit Kapiteln. VLC, Kodi und viele Fernseher zeigen die Kapitel an. Mit „An Schnitten Kapitelmarken einfügen“ beginnt an jedem Schnitt ein Kapitel, dazu kommen eigene Marken bzw. die Kapitel der DVD. VOB- und MPEG-Dateien können grundsätzlich keine Kapitel enthalten.
- „An Schnitten Kapitelmarken einfügen“ ist jetzt sichtbar: In „Alle Schnitte“ zeigt die Zeitleiste die Übergänge als Kapitelmarken, und unter dem Häkchen steht, wie viele Kapitel entstehen.
- „Backup erstellen“ heißt jetzt „Exportieren …“, auch im Dialog („Export starten“, „Export fertig“ usw.).

## 1.0.017 · 2026-11-11 20:17

- Neue Option „An Schnitten Kapitelmarken einfügen“ unter Kapitelmarken: Alle Einträge der Liste „Neue DVD“ laufen als ein Titel nahtlos hintereinander, und jeder Eintrag beginnt ein Kapitel. Weiter und Zurück am Player springen so von Schnitt zu Schnitt. Eigene Marken und die Kapitel der DVD innerhalb der Einträge bleiben erhalten. Einträge mit anderem Bild- oder Tonformat beginnen einen neuen Titel. Die Einstellung wird im Projekt gespeichert und gilt für DVD-Ordner, ISO und Brennen.

## 1.0.016 · 2026-11-11 18:18

- Umschalt+Klick in der Liste „Neue DVD“ rechnet den Bereich jetzt immer vom zuletzt ohne Umschalt geklickten Eintrag aus, wie im Explorer. Bisher galt der vorige Umschalt+Klick als Anfang, und ein weiterer Umschalt+Klick markierte nur den Rest.

## 1.0.015 · 2026-11-11 18:06

- Mehrfachauswahl in der Liste „Neue DVD“: Strg+Klick wählt einzelne Einträge dazu oder ab, Umschalt+Klick einen Bereich. Entfernen (Papierkorb, Rechtsklick-Menü) gilt dann für alle gewählten.
- Entf-Taste entfernt den gewählten Eintrag bzw. alle gewählten.
- Rückgängig: Strg+Z oder der neue Pfeil-Knopf unter der Liste holt die zuletzt entfernten Einträge an ihre alte Stelle zurück.

## 1.0.014 · 2026-11-11 17:40

- Vorschau „Ganzer Titel“: Beim Wechsel zu einem geschnittenen Eintrag steht der Zeitleisten-Punkt jetzt am Anfang des Schnitts (grüner Rahmen) statt am Titelanfang.

## 1.0.013 · 2026-11-11 17:01

- Blockartefakte an bildgenauen Schnitten behoben: Die neu kodierten Bilder einer angeschnittenen Bildgruppe bekamen nur ihren Anteil am Original. Blieb ein einziges Bild übrig, musste es als I-Bild mit einem Zwölftel auskommen und wurde grob (Quantisierer bis 19). Jetzt bekommt das erste Bild so viel wie das I-Bild im Original, begrenzt durch die DVD-Datenrate; in den Tests liegt der Quantisierer überall bei 2 bis 5.
- Zeitsprünge an Schnittstellen in MPEG- und VOB-Dateien behoben: Ließ ein angeschnittener Block der Systemuhr wenig Spielraum, wurde der ganze nächste Eintrag um bis zu 0,2 s verschoben. Jetzt rückt nur die Systemuhr weiter. Außerdem werden Abstände ab 30 ms zwischen zwei Szenen ausgeglichen (bisher erst ab 0,5 s), sonst entstand eine kurze Pause in Bild und Ton.
- „Von ursprünglich …“ im Abschlussfenster zählt beim Neu-Zusammenstellen nur die geschnittenen Teile, nicht für jeden Eintrag den ganzen Film.

## 1.0.012 · 2026-11-11 11:24

- Einzelne VOB- und MPEG-Dateien öffnen: neue Karte „VOB-/MPEG-Datei“ im Startbild (auch per Ziehen und Ablegen und in der Warteschlange). Die Datei wird ohne Neukodierung in eine DVD-Struktur umgepackt; danach gehen Vorschau, Schnitt, Szenenerkennung, Blenden und alle Ausgabearten wie bei einer DVD. „Ganze DVD“ entfällt dabei, der Film steht gleich in der Liste.
- Unterstützt werden MPEG-2-Programmströme mit AC-3-, MPEG- oder LPCM-Ton. Untertitel und DTS-Ton werden nicht übernommen. Die umgepackte Fassung liegt im Temp-Ordner und wird beim erneuten Öffnen derselben Datei wiederverwendet.
- Schnittstellen: Ein zufälliges Sync-Muster im Ton konnte am Anfang eines Ausschnitts für einen Rahmenanfang gehalten werden; dann blieb ein angeschnittener Tonrahmen stehen. Behoben.

## 1.0.011 · 2026-11-11 11:03

- Zeitleiste bei „Alle Schnitte“ und „Übergang anhören“: Beim Wechsel zum nächsten Eintrag sprang der Punkt kurz ans rechte Ende, bis das erste Bild geladen war. Er steht jetzt sofort an der Schnittstelle.

## 1.0.010 · 2026-11-11 10:44

- „Übergang anhören“ sprang nur an den Anfang des Titels. Sprungstelle und Start gingen an die alte Vorschau und beim Neuladen verloren. Jetzt spielt der Knopf wie gedacht 2,5 s vor bis 2,5 s nach der Schnittstelle und hält dann an.

## 1.0.009 · 2026-11-11 10:24

- Mini-Blende: Der Ton wird an jeder Schnittstelle 15 ms weich aus- und eingeblendet, damit der harte Übergang zwischen zwei Szenen nicht knackt. Abschaltbar im Backup-Dialog.
- Übergang anhören: Neuer Knopf (Ohr) unter der Liste „Neue DVD“ und im Rechtsklick-Menü. Spielt je 2,5 s vor und nach der Schnittstelle zum nächsten Eintrag und hält danach an.
- Ausgabe prüfen: Nach dem Backup wird die Ausgabe gelesen und auf beschädigte Tonrahmen und Zeitsprünge im Ton untersucht. Das Ergebnis steht im Abschlussfenster, mit den Stellen im Film.
- Übersichtsbild: Auf Wunsch entsteht neben der Ausgabe eine JPG-Datei mit Vorschaubildern im einstellbaren Zeitabstand (Raster mit vier Spalten, Zeitangabe je Bild).
- Schnitt: Liegt der Ton einer DVD weit hinter dem Bild, begann der Ton des nächsten Eintrags bis zu 0,1 s zu früh. Behoben.

## 1.0.008 · 2026-11-10 19:49

- Knacken im Ton an Schnittstellen behoben (vor allem in Kodi hörbar): Am Anfang und Ende jedes Eintrags blieben angeschnittene Tonrahmen stehen. Sie werden jetzt aus den Paketen entfernt, der Ton beginnt und endet mit vollständigen Rahmen (AC-3 und MPEG-Ton).
- Bildgenauer Schnitt: Ton am Ende eines Abschnitts ging verloren, weil er auf vielen DVDs etwas nach dem Bild im Datenstrom liegt. Er wird jetzt mitgenommen, auch aus dem Block hinter dem Schnitt.
- Bildgenauer Schnitt: Die Systemuhr eines angeschnittenen Blocks rückt mit dem Schnitt mit. Bisher entstand dadurch beim Aneinanderfügen eine Lücke von bis zu einer halben Sekunde in Bild und Ton.
- Ton und Bild treffen an jeder Schnittstelle auf etwa einen Tonrahmen genau zusammen.

## 1.0.007 · 2026-11-10 19:12

- Überschriebene Dateien (MPEG, VOB, ISO, DVD-Ordner, Projekte) bekommen ein neues Erstelldatum. Windows behielt beim Überschreiben bisher das alte, und der Explorer zeigte deshalb die Uhrzeit der ersten Ausgabe.

## 1.0.006 · 2026-11-10 18:02

- Neuer Knopf „Auf alle Einträge übertragen“ bei den Blenden: übernimmt Ein- und Ausblenden, Dauer und Ton-Blende für alle anderen Einträge, z. B. alle Szenen nach der Szenenerkennung. Weil Blenden nur bei bildgenauem Schnitt wirken, wird das dort mit eingeschaltet.

## 1.0.005 · 2026-11-10 17:36

- Neuer Projektordner in den Einstellungen: Projekte öffnen und speichern standardmäßig dort. Wählt man einen anderen Ort, gilt dieser bis zum Neustart.
- Die Meldung „Projekt gespeichert“ zeigt die Uhrzeit der Speicherung (der Explorer zeigt nur Minuten, zweimal Speichern in derselben Minute sah dort aus wie keine Änderung).
- Der Hinweis „Vorschau wird geladen“ im Hauptbild entfällt; bis das Bild da ist, bleibt die Fläche dunkel.

## 1.0.004 · 2026-11-10 15:01

- MPEG- und VOB-Datei enden mit einem kleinen Abschlusspaket, das die Endzeit trägt. VLC sucht die Länge nur in den letzten 200 KB der Datei; viele DVDs tragen Zeitstempel aber nur alle halbe Sekunde ein, und bei einer großen letzten Bildgruppe fand VLC keinen. Es schätzte die Länge dann aus der Datenrate und zeigte z. B. 8:35:37 statt 21:04.

## 1.0.003 · 2026-11-10 13:37

- Falsche oder negative Gesamtlänge in VLC bei einzelnen Kapiteln (z. B. nach „In Einzeltitel zerlegen“) behoben: Beginnt ein Kapitel mitten im Film, liegt sein Ton etwas vor dem ersten Bild. Dieser Ton rutschte bisher unter 0:00 und ließ die Uhr der Datei überlaufen.

## 1.0.002 · 2026-11-10 11:54

- MPEG- und VOB-Datei beginnen jetzt immer bei 0:00. Bisher blieb die Uhrzeit der DVD erhalten; bei manchen DVDs lief sie dadurch über, und Player wie VLC zeigten eine falsche oder negative Gesamtlänge.

## 1.0.001 · 2026-11-10 11:28

Erste Version mit Versionsnummer. Enthält:

- DVDs verkleinern: Bild wird neu quantisiert statt neu kodiert, Menüs und Struktur bleiben (Ganze DVD).
- Neu zusammenstellen: Titel und Kapitel in eigener Reihenfolge, Ausgabe als Ordner, ISO, VOB, MPEG-2 oder direkt auf Rohling.
- Schnitt am Schlüsselbild ohne Neukodierung oder bildgenau.
- Blenden aus/nach Schwarz oder Weiß bei bildgenauem Schnitt, Ton auf Wunsch mitgeblendet (AC-3, MPEG-Ton, LPCM).
- Szenenerkennung: Eintrag an Szenenwechseln teilen, Szenen mit Vorschaubild auswählen.
- Kapitel-Editor mit eigenen Kapitelmarken.
- Projekte speichern und laden, Warteschlange für mehrere DVDs.
- Vorschau mit Ton, Zeitleiste aktualisiert das Bild schon beim Ziehen.
- Oberfläche auf Deutsch und Englisch.
- Versionsnummer oben links, Änderungsliste unter „Über“.
