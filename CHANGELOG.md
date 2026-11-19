# Änderungen

Jede Version zählt die letzte Stelle hoch. Neueste Version oben.

## 1.0.051 · 2026-11-19 11:11

- MKV mit allen Spuren: Der MKV-Export übernimmt jetzt alle in den Einträgen angehakten Tonspuren und die DVD-Untertitel (als VobSub, mit Bildgröße und Farbpalette der DVD), jeweils mit Sprache und Namen („Deutsch“, „Englisch“). Die erste Tonspur ist voreingestellt, Untertitel sind zuschaltbar. Die Auswahl einer einzelnen Tonspur gibt es nur noch bei MPEG-2.
- Namensvorlage beim Export als Einzeldateien: Ein Feld „Dateinamen“ mit Platzhaltern {Name} (Titelname), {Nr} (laufende Nummer, zweistellig) und {DVD} (Name der DVD bzw. Datei), z. B. „Serie S01E{Nr} - {Name}“. Bei {Nr} lässt sich die erste Nummer einstellen; gezählt werden nur die gewählten Titel. Der Zusatz „_Neu“ kommt weiterhin ans Ende. Die Vorlage bleibt gespeichert.
- Neu: „Datei prüfen“ (Stethoskop oben rechts). Untersucht VOB- und MPEG-Dateien und meldet mit Zeitangaben: Ton oder Bild kommt zu spät beim Player an (dort stocken Player), verfälschte AC3-Rahmen (Prüfsumme), zerrissene Tonrahmen, Zeitsprünge im Ton und eine rückwärts laufende Systemuhr. Eine 21-Minuten-Folge ist in etwa einer Sekunde geprüft.
- Umpacken von VOB-/MPEG-Dateien: Beginnt der Ton erst deutlich nach dem Bild, wartet das Umpacken jetzt auch am Dateianfang auf ihn; bisher kam er dort zu spät an.

## 1.0.050 · 2026-11-18 10:04

- Umpacken von VOB-/MPEG-Dateien: Der Ton kommt jetzt gleichmäßig 120 ms vor seiner Abspielzeit beim Player an, wie auf gekauften DVDs (dort etwa 100 ms). In 1.0.049 waren es über 260 ms, was den kleinen Tonpuffer mancher Hardware-Player überfüllen kann. Dafür läuft die Systemuhr einer VOBU jetzt gleichmäßig über ihre ganze Dauer, statt am Anfang gedrängt; das Bild kommt 200–260 ms vor seiner Dekodierzeit. Gemessen an einer echten Folge mit zwei Tonspuren: beide Spuren 117–120 ms, nirgends zu spät. Umgepackte Kopien im Zwischenspeicher werden dafür einmal neu angelegt.

## 1.0.049 · 2026-11-18 09:35

- Hänger im Ton bei geöffneten VOB-/MPEG-Dateien behoben: Beim Umpacken kam bei Dateien mit mehreren Tonspuren die zweite Spur fast durchgehend zu spät beim Player an (bis zu 0,2 s nach ihrer Abspielzeit), die erste nur knapp rechtzeitig. Player stockten dann, vor allem auf der zweiten Spur, und der Export übernahm das. Ursache: Der Ton liegt in vielen Quellen ein Stück hinter dem Bild und landete in der nächsten VOBU, dort zudem Spur für Spur statt nach Zeit einsortiert. Jetzt wartet das Umpacken, bis der Ton einer Bildgruppe gelesen ist, verzahnt alle Spuren nach Zeit und hält mindestens 100 ms Vorlauf ein (gemessen an einer echten Folge: jetzt über 260 ms auf beiden Spuren). Alte umgepackte Kopien im Zwischenspeicher werden dafür einmal neu angelegt; bereits exportierte Dateien bitte neu exportieren.

## 1.0.048 · 2026-11-17 17:37

- Mehrere VOB-/MPEG-Dateien auf einmal öffnen: Im Dialog „VOB-/MPEG-Datei“ lassen sich jetzt mehrere Dateien wählen (auch per Ziehen ins Fenster oder „Öffnen mit“). Sie werden zusammen wie eine DVD geöffnet, jede Datei als eigener Titel in der Reihenfolge der Dateinamen. Die Einträge in „Neue DVD“ und die Titel links heißen wie die Dateien; oben steht „3 Dateien“. Projekte merken sich alle Dateien, auch in der Warteschlange.
- Export als Einzeldateien: Neue Option „Zusatz an die Dateinamen hängen“ mit änderbarem Text (Vorgabe „_Neu“). Aus „Hochzeit“ wird so „Hochzeit_Neu.mkv“, und der Export kann neben den gleichnamigen Quelldateien liegen. Die Einstellung bleibt gespeichert. Bei zusammen geöffneten Dateien ist „Jeden Titel als eigene Datei speichern“ gleich angehakt.

## 1.0.047 · 2026-11-17 17:01

- Geöffnete Dateien sind gegen Löschen gesperrt. Solange eine DVD, ein ISO-Abbild oder eine Videodatei geöffnet ist (auch während die Warteschlange sie abarbeitet), lassen sich ihre Dateien im Explorer nicht löschen, umbenennen oder verschieben; Windows meldet dann „Die Datei ist in einem anderen Programm geöffnet“. Gleiches gilt für die geöffnete Projektdatei, die sich weiterhin speichern lässt. Nach dem Schließen oder Beenden ist alles wieder frei. Eingelegte DVDs im Laufwerk werden nicht gesperrt, damit sie sich auswerfen lassen.

## 1.0.046 · 2026-11-17 16:32

- ISO-Abbilder mit einer VOB-Datei von genau 1 GiB werden wieder vollständig gelesen. Manche Brennprogramme tragen diese Länge so ein, dass sie in die Typ-Bits des UDF-Eintrags überläuft. Die Datei (z. B. VTS_01_2.VOB) galt dann als leer und fehlte, alle folgenden Teile rutschten um 1 GiB nach vorn. In der Vorschau sprang der Zeitmarker dadurch plötzlich zurück und das Bild lief im Zeitraffer; auch ein Backup aus so einem Abbild war unvollständig.

## 1.0.045 · 2026-11-17 11:35

- Wechsel zu „Alle Schnitte“ springt nicht mehr an den Anfang des Schnitts. Liegt das gezeigte Bild im Schnitt des Eintrags, läuft die Wiedergabe einfach weiter, wie beim Wechsel zu „Ganzer Titel“. Nur wenn das Bild außerhalb des Schnitts steht, geht es am Schnittanfang weiter, laufend oder angehalten wie zuvor.

## 1.0.044 · 2026-11-17 11:23

- Zoombare Zeitleiste bei „Alle Schnitte“: Sie zeigt jetzt alle Einträge der neuen DVD hintereinander, mit Standbildern, Tonspur und einer Zeitskala über die ganze Länge, so wie die schmale Leiste. Bisher zeigte sie wie bei „Nur Schnitt“ nur den gerade laufenden Eintrag. Grüne Linien markieren die Übergänge zwischen den Einträgen, ein farbiger Streifen den laufenden Eintrag. Klicken springt an die Stelle, auch in einen anderen Eintrag. Beim Darüberfahren steht der Eintrag dabei. Das Kontextmenü setzt Schnittmarken im Eintrag unter dem Zeiger.

## 1.0.043 · 2026-11-17 10:46

- Vorschau-Ton ohne Knacken: Die Zeit jedes Tonstücks wird je Bildgruppe aus der Zellzeit der DVD abgeleitet, und die ist nur bildgenau (bei NTSC zusätzlich gerundet). Weicht sie um ein paar Millisekunden ab, schob die Vorschau eine winzige Stille ein oder übersprang Abtastwerte, was als Knacken zu hören war. Stücke, die bis auf 45 ms an das vorige anschließen, laufen jetzt nahtlos weiter. Echte Sprünge (Schnitte, Springen in der Zeitleiste) bleiben Sprünge. Die Ausgabe war davon nie betroffen.

## 1.0.042 · 2026-11-17 10:32

- Kapitelmarken zeigen jetzt nur, was zum gewählten Eintrag gehört. „Kapitel bearbeiten“ übernimmt nur die DVD-Kapitel im geschnittenen Bereich, nicht mehr alle Kapitel des ursprünglichen Titels. Bei einem ausgeschnittenen Teil beginnst du so mit einer leeren oder passenden Liste und kannst eigene Marken ergänzen.
- Die Markenliste blendet Marken außerhalb des Schnitts aus. Darunter steht, wie viele das sind. Sie kommen wieder, wenn du den Schnitt vergrößerst.
- Der Hinweis zu den DVD-Kapiteln nennt die Zahl im Schnitt („Im Schnitt liegen 2 Kapitel der DVD …“).
- „Marke hier“ und „Neu: Marke hier“ sind nur aktiv, wenn die Vorschau im Schnitt steht. Verschieben mit den Pfeilen oder der Stecknadel hält die Marke im Schnitt.

## 1.0.041 · 2026-11-17 10:18

- Behoben: „Neu: Marke hier“ war bei schmalerem Fenster nicht zu sehen. Einige Zeilen im Bereich rechts waren breiter als die Spalte, dadurch ragte die ganze Spalte aus dem Fenster und Knöpfe am rechten Rand wurden abgeschnitten. Die Zeilen für Schnitt und Kapitelmarken sind jetzt kompakter, und Knopfreihen brechen um, statt abgeschnitten zu werden.
- „Marke hier“ und die Marken-Knöpfe beim Schnitt sind jetzt auch nutzbar, während das nächste Vorschaubild noch lädt (nach einem Sprung oder Neuladen). Sie nehmen dann die angesteuerte Stelle. Der Knopf „Auf die Stelle der Vorschau setzen“ in der Markenliste bleibt immer sichtbar und ist höchstens ausgegraut.
- Neu: „An Kapiteln in Einträge teilen“ unter den Kapitelmarken und im Kontextmenü der Liste. Der Eintrag wird an jedem Kapitelanfang im geschnittenen Bereich geteilt, mit deinen eigenen Kapitelmarken oder sonst den Kapiteln der DVD. Schnitt und Einstellungen bleiben erhalten. Die Einblende bekommt der erste Teil, die Ausblende der letzte, und ein eigener Name wird durchnummeriert („Name – Teil 1“ …).
- Entfernt: „Am Schnitt in Einträge teilen“ und „In … Einzeltitel zerlegen“. Beides geht jetzt über „An Kapiteln in Einträge teilen“.

## 1.0.040 · 2026-11-16 17:06

- Neu: „Am Schnitt in Einträge teilen“ im Bereich Schnitt und im Kontextmenü der Liste. Was vor der Anfangsmarke und nach der Endmarke liegt, wird je ein eigener Eintrag davor und dahinter. Aus einem Titel mit beiden Marken werden so drei Einträge. Der mittlere behält Schnitt, Namen, Blenden und Kapitelmarken und bleibt ausgewählt. Ist nur eine Marke gesetzt, entstehen zwei Einträge.

## 1.0.039 · 2026-11-15 18:03

- „Alle Schnitte abspielen“ über der Liste der neuen DVD spielt jetzt nicht mehr alles von vorn bis hinten, sondern geht die Schnitte durch: Die Vorschau springt kurz vor jeden Schnitt, spielt ihn bis kurz danach ab und springt dann zum nächsten. Dazu gehören der Anfang des ersten Eintrags (wenn er geschnitten ist), jeder Übergang zwischen zwei Einträgen und das Ende des letzten Eintrags (wenn es geschnitten ist). Liegen zwei Stellen dicht beieinander, laufen sie in einem Stück durch. Ohne Schnitte läuft alles wie bisher von vorn bis hinten.

## 1.0.038 · 2026-11-15 12:12

- Die Titelliste im Export sieht jetzt immer gleich aus, mit Standbildern von Anfang und Ende, Unterzeile und Größe. Mit „Jeden Titel als eigene Datei speichern“ steht in jeder Zeile statt des Namens ein Eingabefeld für den Dateinamen. Ohne die Option bleibt der Name fest.

## 1.0.037 · 2026-11-15 11:58

- Lange Titelnamen werden in der Liste „Neue DVD“ und in der Titelliste des Exports sauber mit „…“ gekürzt statt mitten im Buchstaben abgeschnitten. Der volle Name erscheint, wenn die Maus über der Zeile steht.
- Export als VOB, MPEG-2 oder MKV mit nur einem Titel: Auch hier gibt es jetzt die Option „Titel einzeln speichern, Dateiname aus der Liste“. Mit Häkchen wird der Name aus der Titelliste zum Dateinamen, ohne Häkchen gilt wie bisher der Dateiname im Pfad oben.
- „1 Datei“ statt „1 Dateien“ in der Dateiübersicht des Exports.

## 1.0.036 · 2026-11-15 11:37

- Einträge der Liste „Neue DVD“ lassen sich umbenennen: Doppelklick auf den Eintrag, F2 oder „Umbenennen“ im Kontextmenü. Enter oder ein Klick daneben übernimmt den Namen, Esc bricht ab, ein leeres Feld stellt „Titel …“ wieder her. Der Name gilt auch beim Export (Titelliste und Dateinamen der Einzeldateien).

## 1.0.035 · 2026-11-15 10:04

- Export als eine Datei: Die Titelliste zeigt jede Zeile wie die Liste „Neue DVD“, mit Standbildern von Anfang und Ende (bei geschnittenen Einträgen von Schnittanfang und -ende), Name, Kapiteln bzw. Schnittlänge und Größe. Ein Klick auf die Zeile setzt oder entfernt das Häkchen. Bei „Ganze DVD“ stehen dort die Titel der DVD mit Länge und Kapiteln. Mit „Jeden Titel als eigene Datei speichern“ bleiben die Namensfelder wie bisher.

## 1.0.034 · 2026-11-14 18:32

- „Titel dieser DVD“ steht jetzt in der Reihenfolge der DVD (Titel 1, 2, 3 …). Bisher stand der längste Titel als vermeintlicher Hauptfilm ganz oben, was bei DVDs mit mehreren gleich langen Teilen wie eine falsche Reihenfolge aussah. Intern bleibt die Reihenfolge gleich, gespeicherte Projekte passen also weiter.

## 1.0.033 · 2026-11-14 18:11

- Export als Datei: Ohne „Jeden Titel als eigene Datei speichern“ zeigt die Liste nur noch Häkchen mit dem Titelnamen als Text, ohne Namensfelder. Die Namen werden nur für Einzeldateien gebraucht; mit der Option erscheinen die Felder wie bisher.

## 1.0.032 · 2026-11-14 17:57

- Zwischenablage: Einfügen (Strg+V), Kopieren (Strg+C) und Ausschneiden (Strg+X) funktionieren jetzt in allen Textfeldern, etwa bei den Titelnamen im Export. Die Anbindung an die Windows-Zwischenablage war bisher nicht eingebaut.

## 1.0.031 · 2026-11-14 17:39

- Zeitleiste: Die Standbilder liegen jetzt bei jeder Zoomstufe lückenlos nebeneinander. Bisher standen sie auf festen Zeitabständen, so dass je nach Zoom fast eine Kachel breite Lücken blieben. Jetzt zeigt jede Kachel das Bild an ihrem Anfang (auf ein feines Zeitraster gerundet, damit schon fertige Bilder beim Zoomen weiterverwendet werden). Solange ein Bild noch fehlt, steht dort etwas abgedunkelt ein fertiges Bild aus der Nähe statt einer leeren Fläche.

## 1.0.030 · 2026-11-14 17:17

- Tonspur in der Zeitleiste deutlicher: Bisher war sie logarithmisch (in dB) dargestellt, bei Filmton sah das fast überall gleich hoch aus wie eine breite Linie. Jetzt:
  - linear und auf den lautesten Ausschlag des Titels bezogen, so dass leise und laute Stellen klar unterschiedlich hoch sind;
  - außen hell der Spitzenpegel, innen kräftig der mittlere Pegel (RMS), der zeigt, wie laut es tatsächlich ist; Dialoge, Musik und Pausen heben sich so voneinander ab;
  - das Band ist höher (56 statt 36 Bildpunkte).

## 1.0.029 · 2026-11-14 17:08

- Tonspur in der Zeitleiste: Unter den Standbildern zeigt ein Band den Lautstärkeverlauf der Tonspur, die auch die Vorschau abspielt (Spitzenpegel von −48 dB bis 0 dB, um die Mittellinie gespiegelt). Pausen und Szenenwechsel sind so beim Schneiden gut zu erkennen.
  - Ein- und ausschalten mit dem Knopf „Tonspur“ in der Werkzeugzeile der Zeitleiste; die Wahl bleibt gespeichert.
  - Für den Verlauf wird der Titel einmal im Hintergrund ganz gelesen. Bis er fertig ist, füllt sich das Band von links, und rechts steht der Fortschritt. Das Ergebnis bleibt für die zuletzt gezeigten Titel erhalten.
  - Weggeschnittene Bereiche sind auch in der Tonspur abgedunkelt.

## 1.0.028 · 2026-11-14 16:56

- Die zoombare Zeitleiste steht jetzt direkt unter dem Vorschaubild, an Stelle der schmalen Leiste. Der Knopf „Zeitleiste“ schaltet zwischen beiden um, es ist immer nur eine zu sehen; die Wahl bleibt gespeichert. Beim Ziehen in der Zeitleiste zeigt die Zeitangabe die Stelle unter dem Zeiger.

## 1.0.027 · 2026-11-14 16:44

- Zoombare Zeitleiste mit Standbildern unter der Vorschau, zum genauen Setzen von Schnitten:
  - Oben eine Zeitskala, darunter eine Reihe kleiner Standbilder der jeweiligen Stellen. Abspielposition, Schnittmarken (grün, Weggeschnittenes abgedunkelt) und Kapitelmarken liegen darüber.
  - Zoomen mit dem Mausrad (um die Mausposition), mit dem Schieberegler oder mit Lupe +/−, bis hin zu einzelnen Bildern. Der Knopf daneben zeigt wieder die ganze Länge.
  - Verschieben mit Umschalt+Mausrad, mit der mittleren Maustaste oder mit dem Rollbalken darunter. Beim Abspielen folgt der Ausschnitt der Abspielposition.
  - Klicken oder Ziehen setzt die Abspielposition, beim Loslassen bildgenau. Der Rechtsklick setzt wie auf der schmalen Leiste Schnittanfang oder -ende an dieser Stelle.
  - Die Standbilder entstehen im Hintergrund, die nächstgelegenen zuerst, und bleiben zwischengespeichert. Ihre Abstände verdoppeln bzw. halbieren sich beim Zoomen, damit schon fertige Bilder wiederverwendet werden.
  - Ein- und ausklappen mit dem Knopf „Zeitleiste“ unter der Vorschau; die Wahl bleibt gespeichert. Die bisherige schmale Leiste bleibt unverändert als Übersicht erhalten.

## 1.0.026 · 2026-11-14 12:51

- Schnelleres Einlesen einer DVD:
  - Die Titel-Vorschaubilder im Ladefenster entfallen. Sie haben parallel zur Analyse an ganz anderen Stellen der DVD gelesen und den Lesekopf ständig springen lassen.
  - Der eigene Durchgang zur Kopierschutz-Prüfung vor der Analyse entfällt. Die Analyse erkennt den Kopierschutz selbst, die DVD wird also nur noch einmal abgefahren.
  - Die Schnellanalyse nimmt weniger Stichproben: Der größte Titelbereich bekommt weiter 24, kleinere entsprechend weniger, Menüs 4 statt 8. Bei einer Test-DVD mit 44 Titelsets sind das rund 320 statt 860 Lesezugriffe, bei praktisch gleichen Größenangaben.
  - Dadurch kommt auch das mitlaufende Bild früher. Der Fortschrittsbalken zählt jetzt die tatsächlichen Lesezugriffe und läuft dadurch gleichmäßiger.

## 1.0.025 · 2026-11-14 12:30

- Mitlaufendes Vorschaubild: Beim Einlesen einer DVD, eines Ordners oder einer VOB-/MPEG-Datei und beim Export zeigt ein kleines Bild die Stelle, die gerade verarbeitet wird. Es aktualisiert sich etwa jede Sekunde. Eingeschaltet wird es mit dem Knopf „Bild anzeigen“ im jeweiligen Fenster; standardmäßig ist es aus, und die Wahl bleibt fürs nächste Mal gespeichert. Ausgeschaltet kostet es keine Rechenzeit.

## 1.0.024 · 2026-11-14 11:17

- Dateiendung für MPEG-2: Neben .mpg und .MPG stehen jetzt auch .mpeg und .MPEG zur Wahl (Einstellungen → Export). Eine eingetippte .mpg- oder .mpeg-Endung wird passend ersetzt, statt eine zweite anzuhängen.

## 1.0.023 · 2026-11-14 11:08

- Neue Einstellung „Dateiendung beim Export“ (Einstellungen → Export): Für VOB, MPEG-2 und MKV getrennt wählbar, ob die Endung klein (.vob, .mpg, .mkv) oder groß (.VOB, .MPG, .MKV) geschrieben wird. Gilt für einzelne Dateien und für den Export als Einzeldateien; eine eingetippte Endung wird entsprechend angepasst.
- Überschreibt man eine Datei, deren Name sich nur in Groß-/Kleinschreibung unterscheidet (film.vob → film.VOB), gilt jetzt die neue Schreibweise. Windows hätte sonst den alten Namen behalten.
- Der Abschnitt „Backup“ in den Einstellungen heißt jetzt „Export“.

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
