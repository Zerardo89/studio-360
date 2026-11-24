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
- Cartella locale o della fotocamera USB (ricerca ricorsiva per la fotocamera). La griglia mostra foto JPG, JPEG,
  PNG e INSP e video MP4, MOV e INSV. I filtri Foto e Video selezionano il tipo di file.
- Rapporto 2:1 con tolleranza ±0,12 = panorama 360 (stimato); le immagini più larghe di 4096 px vengono ridotte per l'editor.
- Miniature fino a 160 px (stimato), caricate in background e memorizzate in cache per percorso, data di modifica e dimensione
  del file. Per una coppia INSV la cache tiene conto di entrambi i file. L’album mostra soltanto la scheda della lente `_00_`.
- La miniatura del video usa il fotogramma intorno a 1 s (stimato) e mostra un’icona di riproduzione e la durata `m:ss`.
- Filtri per tipo (`album_type_*`) e sorgenti (`album_source_*`), più "Locale" ed "Esempi" per il PC. Stella per i preferiti.

## Video 360

Windows Media Foundation legge i contenitori MP4 e MOV con i codec installati sul PC. Per un INSV usa il gestore MP4 sul
flusso di byte originale. Il Source Reader esegue la conversione in RGB32 e l’app consegna i fotogrammi in RGBA a 8 bit.
Se manca il decoder HEVC, l’editor indica le Estensioni video HEVC di Windows. Ogni worker inizializza e rilascia COM e Media Foundation.

L’editor apre il video in pausa sul primo fotogramma. Durante la riproduzione l’anteprima usa i timestamp del video e applica
yaw, pitch, FOV, colore, filtro e ritaglio come per una foto. La barra temporale permette di spostarsi nel filmato.
Il seek di Media Foundation parte dal fotogramma chiave disponibile e l’app avanza fino a mezzo fotogramma dalla posizione
richiesta (tolleranza stimata). Se manca il frame rate dai metadati si usa 30 fps (stimato).
La riproduzione nell’editor mostra la traccia video. La scheda Esporta offre l’MP4 del montaggio e il salvataggio
del fotogramma corrente in PNG o JPG. Auto Edit mette in pausa e analizza il video dall’inizio.

`--open <file>` apre una foto o un video in Modifica. Con `--tab` esplicito si mantiene la scheda iniziale richiesta.
Per uno screenshot in Modifica l’app attende l’anteprima del primo fotogramma con un limite di 20 s (stimato).

## Doppio fisheye

Un INSP viene decodificato come JPEG e trattato come doppio fisheye affiancato. Per altre immagini e per i video a un solo
flusso si cercano le zone nere intorno ai cerchi e zone illuminate all’interno. I panorami 2:1 già equirettangolari passano
direttamente al visore. Un INSV con due flussi video usa le prime due tracce. Per i file separati si abbinano i nomi che
differiscono per `_00_` e `_10_`. I fotogrammi delle due lenti si abbinano per timestamp, entro 0,75 intervalli tra fotogrammi
(stimato).

La conversione usa due assi opposti con la prima lente orientata a yaw 0° e la seconda a 180°. Ogni lente usa la proiezione
equidistante `r = R × angolo / semi-FOV`. Nella fascia di sovrapposizione l’app interpola i colori fra le lenti con una
curva cubica `smoothstep` e campionamento bilineare. L’approssimazione usa una geometria comune alle fotocamere.
Giunzioni visibili, specialmente su soggetti vicini, possono dipendere da parallasse, orientamento o calibrazione della lente.

| Parametro stimato | Valore |
|---|---|
| FOV di una lente | 200° |
| Diametro del cerchio | 1,0 × il lato minore dell’area della lente |
| Fascia di sovrapposizione | 20° |
| Limite di nero per ciascun canale RGB | 24 su 255 |
| Sonde agli angoli | ±0,43 della larghezza e dell’altezza dal centro, almeno 3 nere su 4 per lente |
| Sonde interne | Centro e ±0,24 lungo gli assi, almeno 2 illuminate su 5 per lente |

## Visore e reframe (Modifica)
- Proiezione rettilineare dal panorama equirettangolare; trascinamento 0,2°/px, inclinazione ±89°, rotella ±5° (stimati).
- Livelli di visuale (`editor_fov_gear_*`): Narrow 60°, Linear 80°, Linear+ 90°, Wide 100°, UltraWide 120°,
  MegaView 140°, Tiny Planet 270° (stereografica), Custom 30–160°. **Gradi stimati**: il titolare non li conosce e
  il codice che li contiene è protetto.
- Tiny Planet: centro bloccato verso il suolo, pitch −90° (fonte: stringa "Center locked to ground by default in Tiny Planet").
- 13 regolazioni colore (`editor_color_adjust_*`) da −100 a +100, partenza 0 (formule approssimate, stimate).
- 14 gruppi di filtri; ritagli 16:9, 9:16, 1:1, 4:3, 3:4, 3:2, 2:3, 5:7, 7:5, 2.35:1, 1:2.35 (fonte: stringhe).
- Anteprima 960×540, 640×360 in modalità prestazioni, 480×270 durante il trascinamento o in compatibilità (stimato).

## Auto Edit foto

Le stringhe dell’app Android descrivono l’analisi di soggetti, scene e momenti salienti di un video 360
(`Auto Edit`, `Analyzing subjects, scenes, and highlights...`, `Analyzing... %d%%`). Indicano inoltre un elenco
di risultati (`Highlights`), la modifica dell’inquadratura (`Edit View`), la selezione e l’esportazione
(`Selected Clips (%d)`, `Select at least one clip`, `Export`) e il caso senza altri soggetti
(`No more targets in this video`).

La ricostruzione analizza una foto equirettangolare sul PC con calcoli deterministici, senza rete o modelli
esterni. Riduce l’RGB con un filtro triangolare, senza ingrandire le foto piccole. La salienza combina Sobel
sulla luminanza e colorfulness di Hasler–Süsstrunk in un vicinato 3×3. L’attività locale attenua le zone
uniformi. Il coseno della latitudine compensa lo stiramento e si applicano pesi aggiuntivi ai poli e al cielo
chiaro a basso gradiente. La mappa passa in continuità tra −180° e 180°.

L’analisi valuta 96 inquadrature con la stessa proiezione rettilineare 16:9 del renderer. Interpola la
salienza e ne calcola la media pesando le celle per l’angolo solido (`1 / lunghezza_raggio³`). Ordina le
viste per salienza e applica entrambe le soglie e la separazione circolare. Il punteggio è la percentuale della
migliore vista, arrotondata. Per ogni vista, le quote di colore determinano la categoria in quest’ordine
**Notte, Neve, Tramonto, Natura, Spiaggia/Mare, Città/Interni**. Il filtro esistente associato alla categoria
compare come suggerimento.

Per il colore, l’istogramma di luminanza e le medie RGB del panorama pesano i pixel per il coseno della
latitudine. L’esposizione usa il rapporto logaritmico tra la mediana desiderata e quella misurata. Il contrasto
usa la differenza dal divario desiderato tra il 5° e il 95° percentile. Il gray-world stima temperatura
e tinta dalle differenze tra le medie dei canali dopo la correzione di esposizione. Saturazione e vividezza
crescono al diminuire della colorfulness. Le regolazioni automatiche arrotondate sostituiscono i sei cursori
interessati quando l’opzione è attiva. Disattivarla ripristina i loro valori precedenti.

Le miniature si aprono nell’editor con un clic. L’annullamento ripristina vista, cursori, filtro e ritaglio
precedenti all’analisi. L’esportazione salva un’immagine per ogni vista selezionata in ordine di punteggio,
con il ritaglio, il colore, il filtro e il profilo di esportazione correnti. `--tab modifica --auto-edit`
avvia il lavoro all’apertura. Con `--screenshot` la cattura attende anche le miniature.

Tutti i valori della tabella sono **stimati** per la ricostruzione desktop. Le scale di proiezione e luminanza
sono condivise con il renderer (luminanza `0,299 R + 0,587 G + 0,114 B`).

| Parametro | Valore | Uso |
|---|---|---|
| `analysis_size` | 512 × 256 px | Dimensione massima dell’immagine analizzata |
| `view_grid` | 32 × 18 campioni | Griglia di proiezione per ogni candidata |
| `thumbnail_size` | 256 × 144 px | Risoluzione del render delle miniature |
| `full_turn` | 360 ° | Periodo dello yaw |
| `yaw_step` | 15 ° | Passo dello yaw, 24 posizioni da −180° a 165° |
| `pitches` | -30, -15, 0, 15 ° | Inclinazioni candidate |
| `view_fov` | 90 ° | Campo visivo orizzontale rettilineare |
| `min_yaw_distance` | 60 ° | Distanza circolare minima tra i risultati |
| `max_highlights` | 4 viste | Numero massimo di risultati |
| `max_score` | 100 punti | Punteggio assegnato al candidato migliore |
| `min_raw_score` | 0,0018 | Soglia assoluta di salienza media |
| `min_relative_score` | 0,34 | Soglia rispetto alla salienza della vista migliore |
| `sobel_scale` | 4 | Divisore del modulo del gradiente Sobel |
| `contrast_weight` | 0,75 | Contributo del gradiente alla mappa di salienza |
| `colorfulness_weight` | 0,25 | Contributo della colorfulness alla mappa di salienza |
| `colorfulness_mean_weight` | 0,3 | Contributo della media cromatica nella metrica Hasler–Süsstrunk |
| `full_activity` | 0,095 | Gradiente o deviazione cromatica necessari per dare peso pieno al vicinato |
| `pole_latitude_degrees` | 60 ° | Latitudine assoluta oltre la quale si applica il peso polare |
| `pole_weight` | 0,25 | Peso aggiuntivo vicino ai poli |
| `sky_min_latitude_degrees` | 5 ° | Latitudine minima per il criterio del cielo |
| `sky_min_luma` | 0,58 | Luminanza minima per il criterio del cielo |
| `sky_max_gradient` | 0,1 | Gradiente massimo per il criterio del cielo |
| `sky_max_chroma` | 0,12 | Croma massimo per identificare cielo chiaro non blu |
| `sky_weight` | 0,18 | Peso delle aree classificate come cielo chiaro |
| `night_max_luma` | 0,23 | Luminanza sotto cui un campione conta come scuro |
| `night_min_share` | 0,62 | Quota di campioni scuri per Notte |
| `snow_min_luma` | 0,72 | Luminanza minima dei campioni chiari per Neve |
| `snow_max_chroma` | 0,16 | Croma massimo dei campioni chiari per Neve |
| `snow_min_share` | 0,42 | Quota di campioni chiari quasi neutri per Neve |
| `sunset_min_red_green` | 0,11 | Differenza minima R − G per un campione da Tramonto |
| `sunset_min_green_blue` | 0,065 | Differenza minima G − B per lo stesso criterio |
| `sunset_min_share` | 0,27 | Quota di campioni caldi per Tramonto |
| `sea_min_blue_red` | 0,1 | Differenza minima B − R per il criterio del mare |
| `sea_min_green_red` | 0,05 | Differenza minima G − R per il criterio del mare |
| `sand_min_luma` | 0,54 | Luminanza minima per il criterio della sabbia |
| `sand_max_red_green` | 0,11 | Limite esclusivo di R − G per la sabbia, con differenza non negativa |
| `sand_min_green_blue` | 0,075 | Differenza minima G − B per il criterio della sabbia |
| `beach_min_share` | 0,47 | Quota con criterio mare o sabbia per Spiaggia/Mare |
| `nature_min_green_advantage` | 0,035 | Vantaggio minimo del verde sul maggiore tra rosso e blu |
| `nature_min_share` | 0,26 | Quota di campioni verdi per Natura |
| `luma_bins` | 256 livelli | Numero di classi dell’istogramma di luminanza |
| `low_percentile` | 0,05 | Estremo basso per il calcolo del contrasto |
| `median_percentile` | 0,5 | Mediana per il calcolo dell’esposizione |
| `high_percentile` | 0,95 | Estremo alto per il calcolo del contrasto |
| `target_median_luma` | 0,45 | Mediana di luminanza desiderata |
| `min_median_luma` | 1/255 | Mediana minima nel rapporto di esposizione, 1/255 |
| `target_luma_spread` | 0,65 | Ampiezza desiderata tra il 5° e il 95° percentile |
| `contrast_gain` | 75 cursore | Correzione di contrasto per unità di scarto dall’ampiezza desiderata |
| `target_colorfulness` | 0,24 | Soglia per il recupero della colorfulness |
| `saturation_gain` | 25 cursore | Incremento massimo di saturazione per una scena priva di colore |
| `vibrance_gain` | 40 cursore | Incremento massimo di vividezza per una scena priva di colore |
| `slider_full_scale` | 100 cursore | Valore assoluto del fondo scala dei cursori |
| `slider_limit` | 60 cursore | Limite assoluto applicato al colore automatico |
| `exposure` | 0 | Cursore di esposizione |
| `contrast` | 2 | Cursore di contrasto |
| `saturation` | 3 | Cursore di saturazione |
| `vibrance` | 4 | Cursore di vividezza |
| `temperature` | 11 | Cursore di temperatura |
| `tint` | 12 | Cursore di tinta |
| `color_sliders` | 0, 2, 3, 4, 11, 12 | Ordine dei cursori gestiti |
| `resized_progress` | 12 % | Avanzamento dopo il ridimensionamento |
| `salience_progress` | 45 % | Avanzamento dopo la mappa di salienza |
| `color_progress` | 55 % | Avanzamento al termine del colore automatico |
| `analysis_progress_end` | 90 % | Avanzamento dopo la scansione delle candidate |
| `complete_progress` | 100 % | Avanzamento a completamento delle miniature |
| `progress_refresh` | 32 ms | Intervallo di aggiornamento della barra durante l’analisi |
| `temperature_offset` | 0,1 | Ampiezza della variazione di rosso e blu al fondo scala di temperatura, condivisa col renderer |
| `tint_offset` | 0,08 | Ampiezza della variazione di verde al fondo scala di tinta, condivisa col renderer |
| `exposure_stops` | 2 stop | Ampiezza dell’esposizione al fondo scala, condivisa col renderer |
| `scene_filter_suggestions` | Natura → Scenery, Spiaggia/Mare → Scenery, Neve → Cold, Tramonto → Warm, Notte → Night, Città/Interni → City | Associazione tra scene e gruppi esistenti |
| `clips_selected_by_default` | true | Selezione iniziale delle clip |
| `automatic_color_by_default` | false | Attivazione iniziale del colore automatico |
| `batch_filename` | Studio360-{timestamp}-AutoEdit-{clip:02}.{extension} | Nome normale delle clip di un lotto, con numerazione a partire da 01 |

## Auto Edit video

L’analisi lavora su un thread separato. Il lettore prepara i video equirettangolari e dual-fisheye con
lo stesso percorso della riproduzione. Sui video piatti analizza la scena intera. La mappa Sobel e
cromatica, la griglia di viste candidate, le categorie e le miniature riusano i parametri dell’Auto Edit
foto. La differenza RGB media tra campioni consecutivi pesa ciascuna riga per il coseno della latitudine
nei panorami e usa un peso uniforme nel video piatto. Il punteggio temporale somma la salienza della vista
migliore e il movimento ponderato.

| Parametro stimato | Valore |
|---|---|
| Intervallo e dimensioni massime di analisi | 0,5 s, 512 × 256 px |
| Peso del movimento | 0,65 |
| Lisciatura temporale | 3 campioni con pesi 1, 2, 1 |
| Soglia | maggiore tra 0,0025 e 34% del massimo lisciato |
| Spalle del picco | campioni adiacenti sopra il 55% del picco e sopra soglia |
| Segmento e distanza minima | 2–6 s, con almeno 0,5 s tra i segmenti |
| Punteggio mostrato | 0–100 rispetto al picco migliore tra quelli selezionati |
| Inquadratura ai bordi | vista locale se conserva almeno il 55% della salienza della migliore |
| Movimento verso il segmento seguente | 15% del percorso più breve in yaw, pitch e FOV |
| Avanzamento | analisi 0–90%, miniature fino al 100%, aggiornamento UI ogni 32 ms |
| Decodifica sequenziale | riusa il lettore per richieste in avanti entro 1 s |

Il rilevatore sceglie i picchi più forti prima di ordinarli nel tempo. Su una sorgente più breve di 2 s
usa al massimo la durata del file. La miniatura mostra la vista più saliente del segmento. Se la soglia
esclude tutti i campioni, compare «Nessun altro soggetto in questo video». Le caselle scelgono le clip
da includere nella creazione e nell’esportazione separata. I segmenti esportati singolarmente mantengono
l’intervallo rilevato, la velocità 1× e l’audio originale disponibile. I nomi seguono
`Studio360-{timestamp}-AutoEdit-{clip:02}.mp4`.

## Filmato e modelli

FlashCut e le etichette dei modelli vengono dal brief. I profili Liberi sotto sono **stimati** per il
montaggio deterministico sul PC. Il ritmo è la durata di uscita preferita per una clip.

| Modello | Ritmo | Transizioni preferite in ordine | Filtro | Velocità |
|---|---:|---|---|---:|
| Dinamico | 2,4 s | Sfocatura a destra 2, Scorri a sinistra, Zoom sfocato, Fette | Sport | 1,15× |
| Giornata in viaggio | 4,2 s | Scorri a sinistra, Scorri a destra, Dissolvenza in uscita | Paesaggio | 1× |
| A tempo | 2,0 s | Passaggio a destra, Fette, Scorri in basso | Cinema | 1× |
| Energia | 1,8 s | Sfocatura rotante, Zoom sfocato 2, Sfocatura a destra | Sport | 1,35× |
| Ritmo della vita | 4,6 s | Dissolvenza in uscita, Sfocatura a perno | Caldo | 0,9× |
| Giornata d’avventura | 3,4 s | Scorri in alto, Sfocatura a scorrimento, Scorri a sinistra | Paesaggio | 1× |
| Battito | 1,6 s | Fette, Zoom sfocato, Passaggio a destra | Città | 1,25× |
| Avventure | 3,8 s | Sfocatura a perno, Scorri in basso, Dissolvenza in uscita | Creativo | 1,05× |

Le fasce del brief portano agli obiettivi **stimati** di 30, 60, 120, 180 e 210 s. Il modello parte
dai segmenti selezionati con il punteggio più alto, poi li dispone in ordine cronologico. Se serve
più materiale, allunga i tagli entro le finestre rilevate con una ricerca di 32 iterazioni (stimata).
La durata finale dipende dal materiale disponibile e dai secondi condivisi in transizione. Il taglio
finale può scostarsi fino a 0,25 s (stimati) quando il materiale basta. «Cambia modello» ricrea i tagli
dai segmenti selezionati e conserva musica, mixer e regolazioni globali.

La timeline orizzontale mostra le clip del modello. Nei modelli Liberi puoi sostituire o eliminare una clip
e regolarne filtro, velocità, transizione in uscita e movimento tra la vista iniziale e quella finale.
L’anteprima condivide la timeline e il renderer dell’MP4. Usa 640 × 360 o 360 × 640 px e richieste a
24 fps (stimati). I controlli Musica e Mixer hanno effetto sull’esportazione.

| Profilo | Dimensioni 16:9 | Bitrate H.264 stimato |
|---|---:|---:|
| 720p | 1280 × 720 | 6 Mbit/s |
| 1080p | 1920 × 1080 | 12 Mbit/s |
| 2.7K | 2720 × 1530 | 22 Mbit/s |
| 4K | 3840 × 2160 | 40 Mbit/s |

Per 9:16 si scambiano larghezza e altezza e si usa il ritaglio centrale del renderer. Il brief fissa
i 30 fps e il 4K UHD. Le altre dimensioni sono stimate. Il renderer interpola la vista, combina colore,
filtro globale e filtro della clip e limita la sorgente al doppio del lato lungo, con un massimo
di 4096 px (stimato). «Rendering del video… N%» avanza fino al 99% durante la codifica e raggiunge il 100%
quando il file è stato finalizzato e salvato. Annulla interrompe il lavoro e rimuove il file incompleto.
I segmenti già salvati di un lotto rimangono disponibili. `--auto-edit` su video crea di default
Dinamico prima dello screenshot. `--export-movie file.mp4` crea Dinamico a 720p in 16:9,
rilegge il risultato con VideoReader e termina.

## Hub dei modelli

In **Modifica → Auto Edit → Modelli**, scegli una copertina e premi **Usa modello** o **Cambia modello**.
Le categorie sono **In evidenza**, **Viaggio**, **Sport e azione**, **Vlog** e **Ritmo**. In evidenza
riunisce Impulso urbano, Ultima luce, Giornata in viaggio e Dinamico. I modelli sono disponibili sul dispositivo.
Un suggerimento dell’AI personale porta alla copertina corrispondente.

| Modello | Categoria | Tipo | Spazi | BPM | Durata | Rapporto preferito |
|---|---|---|---:|---:|---:|---|
| Dinamico | Sport e azione | Libero | Variabili | 128 | Libera | 16:9 |
| Giornata in viaggio | Viaggio | Libero | Variabili | 110 | Libera | 16:9 |
| A tempo | Ritmo | Libero | Variabili | 120 | Libera | 16:9 |
| Energia | Sport e azione | Libero | Variabili | 140 | Libera | 16:9 |
| Ritmo della vita | Vlog | Libero | Variabili | 90 | Libera | 16:9 |
| Giornata d’avventura | Viaggio | Libero | Variabili | 112 | Libera | 16:9 |
| Battito | Ritmo | Libero | Variabili | 132 | Libera | 16:9 |
| Avventure | Sport e azione | Libero | Variabili | 108 | Libera | 16:9 |
| Impulso urbano | Ritmo | Sync | 6 | 120 | 15 s | 16:9 (bloccato) |
| Cartoline in corsa | Viaggio | Sync | 5 | 100 | 12 s | 16:9 |
| Slancio | Sport e azione | Sync | 6 | 150 | 10 s | 9:16 (bloccato) |
| Piccole cose | Vlog | Sync | 4 | 96 | 10 s | 9:16 (bloccato) |
| Ultima luce | Viaggio | Sync | 4 | 90 | 12 s | 16:9 (bloccato) |
| Cronaca di strada | Vlog | Sync | 5 | 120 | 12 s | 9:16 (bloccato) |
| Oltre la vetta | Sport e azione | Sync | 5 | 120 | 14 s | 16:9 |
| Colpo di cassa | Ritmo | Sync | 6 | 144 | 10 s | 9:16 |

Nei modelli Sync ogni spazio ha una velocità, una transizione in uscita e una durata netta espressa in
battute. Una battuta dura `60 / BPM` secondi. La clip include anche il tratto condiviso con la clip
successiva, così l’inizio di ogni spazio cade sulla battuta successiva prevista dal modello.
La sequenza parte dai segmenti spuntati, ordinati per punteggio decrescente e poi per sorgente e
tempo d’inizio. Quando finiscono, riprende la stessa graduatoria e cerca un inizio diverso nei segmenti
riutilizzati quando c’è margine. La clip resta nell’intervallo rilevato. Una sorgente troppo breve
accorcia lo spazio e anticipa i tagli seguenti. La timeline mostra la durata effettiva accanto a quella prevista.

Seleziona uno spazio in timeline e usa **Metti nello spazio N** in **Segmenti rilevati** per sostituirlo.
Ogni spazio è obbligatorio. La sostituzione usa la durata prevista per lo spazio, anche se la clip
precedente era più corta. **Sincronizza al ritmo** può spostare i singoli tagli su battiti rilevati
vicini se il materiale consente di mantenere la durata totale. I rapporti bloccati si applicano sia
all’anteprima sia all’esportazione del filmato.

**Musica consigliata** avvia la ricerca Openverse con le parole chiave del modello, quando internet
è attivo e il progetto è senza musica. L’aggiunta di un brano richiede una scelta nella libreria.
**Filtro del modello** sospende i filtri delle singole clip. Riattivandolo si ripristinano i valori
precedenti di ciascuna clip.

Per la schermata dimostrativa, `--panel modelli --screenshot` apre la galleria su Impulso urbano
e applica il modello al video dopo l’analisi.

## Transizioni

I quindici tipi del brief sono **Nessuna**, **Dissolvenza in uscita**, **Sfocatura a destra 2**,
**Sfocatura a perno**, **Sfocatura rotante**, **Sfocatura a destra**, **Sfocatura a scorrimento**,
**Passaggio a destra**, **Zoom sfocato**, **Zoom sfocato 2**, **Fette (4 blocchi)**, **Scorri in basso**,
**Scorri a sinistra**, **Scorri a destra** e **Scorri in alto**. La dissolvenza passa per il nero.
Il passaggio scopre la nuova immagine con un bordo verticale, gli scorrimenti spostano le due immagini
e Fette alterna lo scorrimento orizzontale di quattro fasce. Le sfocature usano tre campioni e spostano,
ruotano o riscalano i fotogrammi. Gli estremi della funzione restituiscono i fotogrammi originali.

Le approssimazioni sono **stimate**. Il tratto condiviso dura fino a 0,36 s e al massimo il 25% della
clip più corta. La sfocatura usa spostamento 0,13 del lato, zoom 0,46, moltiplicatori 1,6 e 1,5 per la
seconda variante a destra e lo scorrimento, rotazioni massime di 0,28 e 1 radiante con apertura 0,26.
Nessuna fa un taglio diretto.

## Velocità

Il controllo usa una velocità costante da 0,25× a 4×, come da brief. Le curve Rampa, Rallenta al centro e
Accelera mantengono la stessa durata di uscita di una clip a quella velocità media. Per una posizione
di uscita normalizzata `u`, la posizione sorgente normalizzata è, rispettivamente,
`u + 0,34 u(1−u)(2u−1)`, `u + 0,28 sin(2πu)/(2π)` e `u − 0,34 u(1−u)`. Le ampiezze 0,34 e 0,28
sono **stimate**. «Durata d’uscita» consente di impostare la velocità equivalente.
«Ripristina durata originale» torna a 1× sull’intervallo tagliato. «Applica a tutte le clip»
copia la velocità e l’eventuale curva.

## Audio e musica

Media Foundation legge l’audio disponibile di video, MP3, M4A, AAC, WAV e WMA. I codec installati in Windows
determinano la compatibilità dei singoli file. I pacchetti PCM a 16 bit vengono riportati a 48 kHz stereo
per timestamp con interpolazione lineare. Il mono viene duplicato e nei flussi multicanale si usano i
primi due canali. In esportazione l’audio originale segue i tagli e le sovrapposizioni. Le clip con
velocità costante diversa da 1× o una curva sono silenziose sulla traccia originale. Il mixer applica
i volumi della musica e dell’originale, somma con saturazione e sfuma in chiusura.

Musica può essere Nessuna, un file Locale, una traccia estratta dal video in M4A (AAC) o un brano da
Online. «Punto di inizio» sposta la lettura della
musica. I tagli si possono sincronizzare e risincronizzare sui picchi di incremento dell’energia RMS.
La sincronizzazione sposta i tagli entro i limiti dei segmenti sorgente e si avvia alla creazione
quando il filmato contiene musica e almeno due clip.

| Parametro stimato | Valore |
|---|---|
| Blocco di lettura per estrazione e battiti | 4096 frame stereo |
| Finestra RMS e storico | 480 frame (10 ms), 16 finestre |
| Soglia di onset | maggiore tra 0,012 e 2,4 volte la media dello storico |
| Distanza fra battiti e spostamento massimo del taglio | 0,24 s ciascuno |
| Precisione di ricerca del taglio e variazione da riportare | 0,0001 s, 0,001 s |
| Volumi iniziali e intervallo | 1 per entrambe le tracce, intervallo 0–1 |
| Dissolvenza finale | 0,8 s |
| Bitrate AAC | 192 kbit/s |

Il Sink Writer scrive H.264 e AAC nello stesso MP4. Se la codifica AAC fallisce, l’app ripete
l’esportazione senza audio e ne indica il motivo. L’estrazione AAC produce un file audio separato.

## Contenuti online

In **Modifica → Musica → Online** si può cercare per parole chiave o scegliere una delle sette
categorie rapide. La ricerca anonima interroga `/v1/audio/` di Openverse con `page_size=20` e
`category=music`. Studio360 mostra titolo, autore, durata, licenza e link alla fonte. Il client
accetta CC BY, CC BY-SA, CC0 e Marchio di pubblico dominio e applica lo stesso filtro ai risultati in
cache. La durata del catalogo è in millisecondi. [API Openverse](https://api.openverse.org/v1/)
· [Campi del catalogo](https://docs.openverse.org/meta/media_properties/catalog.html)

**Anteprima** scarica o riapre il brano e riproduce fino a 20 s dall’inizio. Il Source Reader lo
decodifica in PCM stereo e WASAPI lo riproduce sul dispositivo audio predefinito. Windows deve
disporre di un codec per il formato del fornitore. **Usa** associa il percorso in cache e
l’attribuzione al filmato, con i controlli di inizio, ritmo e volume del percorso musicale
esistente. Al salvataggio di un MP4 con musica online il file `<nome>.crediti.txt` riporta titolo,
autore, fonte, licenza e modifiche. Per CC BY-SA, i termini di condivisione si applicano alla
sincronizzazione con immagini in movimento. [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/legalcode.it)

**Sticker** offre 120 emoji in otto categorie con ricerca locale per nome italiano. Quando si
sceglie una voce, il client usa la copia in cache o richiede il PNG Twemoji da 72×72 a jsDelivr.
Il nome usa i codepoint in esadecimale minuscolo, separati da trattini. Dopo un HTTP 404 con
il selettore `fe0f` il client ritenta senza di esso. Il pannello mostra «Twemoji © Twitter/X e
contributori, CC-BY 4.0». Le esportazioni con sticker visibili riportano il credito accanto al
file. [Twemoji](https://github.com/jdecked/twemoji) · [Licenza della grafica](https://github.com/jdecked/twemoji/blob/main/LICENSE-GRAPHICS)

Sticker e testi usano il centro normalizzato nell’area del ritaglio e una dimensione relativa al
lato corto. Il trascinamento sposta il livello nella vista. La rotellina ridimensiona lo sticker
sotto il puntatore e il pannello consente anche di ruotarlo o nascondere tutti gli sticker.
Il testo si rasterizza con `ab_glyph` e i font inclusi in egui, Ubuntu Light e Noto Emoji
monocromatico. Colore e spessore del contorno si possono modificare. Per i video si può scegliere
un intervallo sulla sorgente con inizio incluso e fine esclusa. Il valore iniziale segue l’intera
clip. I livelli si compongono dopo il reframe con alpha e interpolazione bilineare. Nei filmati
ogni clip applica i suoi livelli all’istante sorgente prima della transizione.

**Impostazioni → Contenuti online** attiva Internet per impostazione predefinita e mostra
dimensione della cache, comando di cancellazione e licenze. Il client controlla prima
`%LOCALAPPDATA%\Studio360\cache\music` per ricerche e tracce e `cache\emoji` per i PNG.
I nomi si basano sull’hash FNV-1a a 64 bit dell’URL. Svuotare la cache rimuove anche i file
musicali online selezionati dai filmati aperti. Gli sticker già caricati rimangono in memoria
per la sessione. Senza rete restano utilizzabili i file locali e i contenuti in cache. Il client
usa HTTPS con il TLS nativo di Windows (SChannel) e User-Agent `Studio360/0.1 (app desktop)`.
Invia ai servizi le parole chiave e gli URL di contenuti richiesti. Non usa account, chiavi,
cookie o dati dei file locali. Le richieste e le operazioni di cache girano su worker.

| Parametro | Valore |
|---|---:|
| Timeout HTTP globale | 15 s per richiesta |
| Intervallo minimo tra richieste Openverse | 1 s |
| Ripresa dopo HTTP 429 | `Retry-After` in secondi, 60 s in assenza di un valore numerico (stimato) |
| Risposta di ricerca | 2 MiB (stimato) |
| Download per brano | 64 MiB (stimato) |
| PNG Twemoji | 64 KiB (stimato) |
| Sticker simultanei in download | 4 (stimato) |
| Livelli per sorgente | 32 (stimato) |
| Dimensione di rasterizzazione del testo | 96 px (stimato) |
| Lunghezza del testo | 160 caratteri (stimato) |
| Attesa degli sticker nello screenshot | 12 s (stimato) |

## AI personale

In **Impostazioni → AI personale** si sceglie un servizio, il suo indirizzo e un modello con visione.
L’interruttore è spento per impostazione predefinita. **Prova connessione** invia «Reply with OK»
come richiesta di solo testo. Per Ollama e LM Studio si usa il servizio compatibile OpenAI e si può
lasciare vuota la chiave.

| Servizio | Indirizzo predefinito | Formato |
| --- | --- | --- |
| Compatibile OpenAI, incluso OpenRouter | `https://api.openai.com/v1` | POST `/chat/completions` con JPEG in URL dati e header `Authorization: Bearer` quando è presente una chiave |
| Anthropic | `https://api.anthropic.com/v1` | POST `/messages` con blocchi immagine JPEG in Base64 e header `x-api-key` |
| Google Gemini | `https://generativelanguage.googleapis.com/v1beta` | POST `/models/{modello}:generateContent` con JPEG in Base64 nei dati inline e header `x-goog-api-key` |

L’Auto Edit di foto e video parte dall’analisi locale. Invia al massimo le 12 miniature rettilinee
con il punteggio locale più alto, con lato massimo di 512 px, in JPEG qualità 80. La richiesta
contiene le miniature senza il file originale o i suoi metadati. Il timeout globale è di 90 s e
il corpo della risposta è limitato a 2 MiB. Il modello restituisce un verdetto JSON con punteggio,
scena e selezione delle clip, un filtro, un modello di filmato e un breve titolo in italiano.
Le candidate che il modello non valuta conservano i risultati locali.

Se l’AI è spenta, manca una configurazione valida, internet è disattivato o la richiesta fallisce,
l’Auto Edit mostra il motivo e usa il risultato locale. Il collegamento richiede HTTPS, con HTTP
ammesso per `localhost`, `127.0.0.1` e `[::1]`.

La chiave è cifrata da Windows DPAPI per l’utente corrente in `%APPDATA%\Studio360\ai-key.bin`.
**Rimuovi chiave** elimina il file. `settings.json` contiene solo il servizio, l’indirizzo, il
modello e lo stato dell’interruttore per l’AI. La chiave viaggia nell’header della richiesta e
viene oscurata nei messaggi d’errore HTTP.

## Esportazione immagini
Risoluzioni 720P, 1080P, 2.7K, **4K** (lato lungo 1280, 1920, 2720, 3840 px): il massimo 4K è confermato dal
titolare; i lati in pixel sono stimati. Sopra 1080P avviso "richiederà più tempo" (fonte: stringa).
Formati PNG o JPG (qualità 92, stimata). Filigrana disattivata di default (stimato).

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
