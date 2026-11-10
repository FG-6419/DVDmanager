# Änderungen

Jede Version zählt die letzte Stelle hoch. Neueste Version oben.

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
