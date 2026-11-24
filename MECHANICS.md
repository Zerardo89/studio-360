# Scheda dei valori — Studio 360 (app desktop per fotocamere 360)

Ricostruzione per Windows dell'app di accompagnamento Android (versione 2.36.8), **senza la parte social**
(Explore, community, profili, messaggi, condivisione, account cloud). Fonti usate:

- manifest, `info.json` e stringhe di risorsa dell'APK;
- decompilazione della libreria nativa `libinssocket.so` (arm64) con il motore Deep (Ghidra 12.1.4; gli indirizzi
  qui sotto sono quelli del file, senza lo spostamento di 0x100000 che Ghidra aggiunge);
- risposte del titolare del progetto.

Il codice Java/Kotlin è protetto da un packer: il DEX contiene solo lo stub `com.ashield.Stub`, quindi le
formule dell'editor non sono leggibili e sono marcate **stimato**.

## Navigazione
Schede: Home, Album, Scatto, Modifica, Impostazioni (fonte: `main_tab_*`; Explore e Me sono social e sono stati tolti).
Colore d'accento `#e8bd00` (fonte: `info.json`).

## Collegamento alla fotocamera (decompilato da `libinssocket.so`)
| Funzione | Indirizzo | Regola letta nel codice | Uso nella build |
|---|---|---|---|
| `nativeOpen` | 0x4b29c | host e porta arrivano dal livello Java; se il canale è già aperto restituisce −20016 ("already open") | un solo tentativo Wi-Fi alla volta (pulsante disattivato mentre prova) |
| `CameraSocketIO` (costruttore) | 0x48f38 | conserva porta e timeout passati da `nativeOpen` | indirizzo e timeout come costanti in `home.rs` |
| `open` | 0x490a0 | risolve l'host in IPv4 o IPv6; buffer di ricezione 0x180000 byte (1,5 MiB) | `TcpStream` accetta entrambe le famiglie |
| `connectWithTO` | 0x5289c | connect non bloccante, controllo ogni 100 ms fino alla scadenza | `connect_timeout` (3 s, stimato) |
| `performTimer` | 0x4a7ec | errore ERR_TIMEOUT (−19) dopo 30 000 ms senza dati | non usato: la build verifica solo la raggiungibilità |
| `serve` | 0x497b8 | avvia il thread di servizio e attende a passi di 5 ms, poi 10 ms | il tentativo gira in un thread separato |

L'indirizzo IP e la porta **non** sono costanti nella libreria (li passa il codice Java protetto):
`192.168.42.1:6666` resta **stimato**. Il protocollo dei messaggi è proprietario e non è stato ricostruito.

Collegamenti offerti (scelta del titolare): **USB** (unità rimovibile con cartella `DCIM`, lettere D–Z),
**Wi-Fi** (tentativo TCP descritto sopra) e **cartella locale**.

## Album
- Cartella locale o della fotocamera USB (ricerca ricorsiva per la fotocamera); immagini JPG/PNG in griglia
  (miniature 160 px, stimato).
- Rapporto 2:1 con tolleranza ±0,12 = panorama 360 (stimato); le immagini più larghe di 4096 px vengono ridotte per l'editor.
- Filtri per tipo (`album_type_*`) e sorgenti (`album_source_*`), più "Locale" ed "Esempi" per il PC. Stella per i preferiti.

## Visore e reframe (Modifica)
- Proiezione rettilineare dal panorama equirettangolare; trascinamento 0,2°/px, inclinazione ±89°, rotella ±5° (stimati).
- Livelli di visuale (`editor_fov_gear_*`): Narrow 60°, Linear 80°, Linear+ 90°, Wide 100°, UltraWide 120°,
  MegaView 140°, Tiny Planet 270° (stereografica), Custom 30–160°. **Gradi stimati**: il titolare non li conosce e
  il codice che li contiene è protetto.
- Tiny Planet: centro bloccato verso il suolo, pitch −90° (fonte: stringa "Center locked to ground by default in Tiny Planet").
- 13 regolazioni colore (`editor_color_adjust_*`) da −100 a +100, partenza 0 (formule approssimate, stimate).
- 14 gruppi di filtri; ritagli 16:9, 9:16, 1:1, 4:3, 3:4, 3:2, 2:3, 5:7, 7:5, 2.35:1, 1:2.35 (fonte: stringhe).
- Anteprima 960×540, 640×360 in modalità prestazioni, 480×270 durante il trascinamento o in compatibilità (stimato).

## Esportazione
Risoluzioni 720P, 1080P, 2.7K, **4K** (lato lungo 1280, 1920, 2720, 3840 px): il massimo 4K è confermato dal
titolare; i lati in pixel sono stimati. Sopra 1080P avviso "richiederà più tempo" (fonte: stringa).
Formati PNG o JPG (qualità 92, stimata). Filigrana disattivata di default (stimato).
I video 360 non si esportano in gruppo (fonte: stringa).

## Scatto
Modalità da `capture_mode_*`; esposizione Auto / Priorità ISO / Priorità otturatore / Manuale; nitidezza
Bassa/Media/Alta/Massima; EV da −4 a +4 a passi di 0,3 (stimato). Senza fotocamera l'anteprima è simulata
e il pulsante di scatto è disattivato.

## Impostazioni
App: Lingua, Svuota cache, Tempo di esportazione, Modalità prestazioni, Modalità compatibilità, Filigrana, Info
(salvate in `%APPDATA%\Studio360\settings.json`).
Dispositivo (attive solo con fotocamera): Wi-Fi della fotocamera, Memoria, Firmware, Informazioni, Calibrazione
giroscopio, Spegnimento automatico, Suono, Spia, Anti-sfarfallio, Ripristino di fabbrica.
Il gruppo "Account" è stato tolto (social/cloud).

## Valori ancora sconosciuti
Gradi esatti dei livelli di visuale, formule dei filtri e delle regolazioni, indirizzo e porta reali della
fotocamera, protocollo dei messaggi: stanno nel codice Java protetto o nel protocollo proprietario.
