//! Application-owned UI text and embedded flag artwork. Device names and the
//! established virtual-microphone suffix are deliberately preserved.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "pt-PT")]
    PortuguesePortugal,
    #[serde(rename = "pt-BR")]
    PortugueseBrazil,
    #[serde(rename = "el")]
    Greek,
    #[serde(rename = "es")]
    Spanish,
    #[default]
    #[serde(rename = "en-GB")]
    EnglishBritish,
}
impl Language {
    pub const ALL: [Self; 5] = [
        Self::PortuguesePortugal,
        Self::PortugueseBrazil,
        Self::Greek,
        Self::Spanish,
        Self::EnglishBritish,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::PortuguesePortugal => "Português (Portugal)",
            Self::PortugueseBrazil => "Português (Brasil)",
            Self::Greek => "Ελληνικά",
            Self::Spanish => "Español",
            Self::EnglishBritish => "English (UK)",
        }
    }
    #[cfg(target_os = "linux")]
    pub fn flag(self) -> &'static [u8] {
        match self {
            Self::PortuguesePortugal => include_bytes!("../ui/flags/pt.svg"),
            Self::PortugueseBrazil => include_bytes!("../ui/flags/br.svg"),
            Self::Greek => include_bytes!("../ui/flags/gr.svg"),
            Self::Spanish => include_bytes!("../ui/flags/es.svg"),
            Self::EnglishBritish => include_bytes!("../ui/flags/gb.svg"),
        }
    }
}
macro_rules! catalog {
    ($($key:ident => [$en:literal, $pt:literal, $br:literal, $el:literal, $es:literal]),* $(,)?) => {
        #[derive(Clone, Copy)]
        #[allow(dead_code, reason = "The shared catalog includes labels specific to each platform")]
        pub enum Text { $($key),* }
        impl Language {
            pub fn text(self, key: Text) -> &'static str {
                match key { $(Text::$key => match self {
                    Self::EnglishBritish => $en, Self::PortuguesePortugal => $pt,
                    Self::PortugueseBrazil => $br, Self::Greek => $el, Self::Spanish => $es,
                }),* }
            }
        }
    };
}
catalog! {
    WindowsOutput => ["Processed audio output", "Saída de áudio processado", "Saída de áudio processado", "Έξοδος επεξεργασμένου ήχου", "Salida de audio procesado"],
    WindowsRouteHint => ["For other apps, choose a virtual cable’s playback endpoint here, then select its recording endpoint in those apps.", "Para outras aplicações, escolha aqui a saída de um cabo virtual e selecione a respetiva entrada nessas aplicações.", "Para outros aplicativos, escolha aqui a saída de um cabo virtual e selecione a entrada correspondente nesses aplicativos.", "Για άλλες εφαρμογές, επιλέξτε εδώ την έξοδο ενός εικονικού καλωδίου και μετά την είσοδό του στις εφαρμογές.", "Para otras aplicaciones, elija aquí la salida de un cable virtual y seleccione su entrada en esas aplicaciones."],
    ChooseOutput => ["Choose a processed-audio output first", "Selecione primeiro uma saída de áudio processado", "Selecione primeiro uma saída de áudio processado", "Επιλέξτε πρώτα έξοδο επεξεργασμένου ήχου", "Seleccione primero una salida de audio procesado"],
    LocalProcessing => ["Local microphone noise reduction", "Redução local de ruído do microfone", "Redução local de ruído do microfone", "Τοπική μείωση θορύβου μικροφώνου", "Reducción local de ruido del micrófono"],
    Refresh => ["Refresh", "Atualizar", "Atualizar", "Ανανέωση", "Actualizar"],
    Title => ["Clearwave", "Clearwave", "Clearwave", "Clearwave", "Clearwave"],
    Language => ["Language", "Idioma", "Idioma", "Γλώσσα", "Idioma"],
    Settings => ["Settings", "Definições", "Configurações", "Ρυθμίσεις", "Ajustes"],
    Stopped => ["Stopped", "Parado", "Parado", "Σε παύση", "Detenido"],
    Stopping => ["Stopping…", "A parar…", "Parando…", "Διακοπή…", "Deteniendo…"],
    Processing => ["Processing", "A processar", "Processando", "Σε λειτουργία", "Procesando"],
    Starting => ["Starting…", "A iniciar…", "Iniciando…", "Εκκίνηση…", "Iniciando…"],
    Microphone => ["Microphone", "Microfone", "Microfone", "Μικρόφωνο", "Micrófono"],
    SourceTip => ["Select the microphone to process", "Selecione o microfone a processar", "Selecione o microfone para processar", "Επιλέξτε το μικρόφωνο για επεξεργασία", "Seleccione el micrófono que desea procesar"],
    RefreshTip => ["Refresh microphones and graphics devices", "Atualizar microfones e dispositivos gráficos", "Atualizar microfones e dispositivos gráficos", "Ανανέωση μικροφώνων και συσκευών γραφικών", "Actualizar micrófonos y dispositivos gráficos"],
    Removal => ["Noise removal", "Redução de ruído", "Redução de ruído", "Μείωση θορύβου", "Reducción de ruido"],
    StrengthTip => ["0% bypasses removal; 100% applies the full predicted noise mask", "0% desativa a redução; 100% aplica toda a máscara de ruído prevista", "0% desativa a redução; 100% aplica toda a máscara de ruído prevista", "Το 0% απενεργοποιεί τη μείωση· το 100% εφαρμόζει ολόκληρη την προβλεπόμενη μάσκα θορύβου", "El 0% desactiva la reducción; el 100% aplica toda la máscara de ruido prevista"],
    Signal => ["Denoised signal", "Sinal sem ruído", "Sinal sem ruído", "Σήμα χωρίς θόρυβο", "Señal sin ruido"],
    NoSignal => ["No signal", "Sem sinal", "Sem sinal", "Χωρίς σήμα", "Sin señal"],
    VirtualMicrophone => ["Virtual microphone", "Microfone virtual", "Microfone virtual", "Εικονικό μικρόφωνο", "Micrófono virtual"],
    ChooseMicrophone => ["Select a microphone above", "Selecione um microfone acima", "Selecione um microfone acima", "Επιλέξτε ένα μικρόφωνο παραπάνω", "Seleccione un micrófono arriba"],
    Default => ["Use as system microphone", "Usar como microfone do sistema", "Usar como microfone do sistema", "Χρήση ως μικρόφωνο συστήματος", "Usar como micrófono del sistema"],
    DefaultTip => ["Apps using a fixed input must select the virtual microphone in their own settings", "As aplicações com uma entrada fixa têm de selecionar o microfone virtual nas respetivas definições", "Os aplicativos com uma entrada fixa precisam selecionar o microfone virtual nas próprias configurações", "Οι εφαρμογές με σταθερή είσοδο πρέπει να επιλέξουν το εικονικό μικρόφωνο στις ρυθμίσεις τους", "Las aplicaciones con una entrada fija deben seleccionar el micrófono virtual en sus propios ajustes"],
    Start => ["Start", "Iniciar", "Iniciar", "Έναρξη", "Iniciar"],
    Stop => ["Stop", "Parar", "Parar", "Διακοπή", "Detener"],
    Close => ["Close", "Fechar", "Fechar", "Κλείσιμο", "Cerrar"],
    ModelFolder => ["Model folder or installer", "Pasta do modelo ou instalador", "Pasta do modelo ou instalador", "Φάκελος μοντέλου ή πρόγραμμα εγκατάστασης", "Carpeta del modelo o instalador"],
    Installer => ["Installer…", "Instalador…", "Instalador…", "Εγκατάσταση…", "Instalador…"],
    SelectingModel => ["Selecting or importing model…", "A selecionar ou importar o modelo…", "Selecionando ou importando o modelo…", "Επιλογή ή εισαγωγή μοντέλου…", "Seleccionando o importando el modelo…"],
    InstallerTitle => ["Select RTX Voice installer", "Selecionar instalador do RTX Voice", "Selecionar instalador do RTX Voice", "Επιλογή προγράμματος εγκατάστασης RTX Voice", "Seleccionar instalador de RTX Voice"],
    Browse => ["Browse…", "Procurar…", "Procurar…", "Αναζήτηση…", "Examinar…"],
    Device => ["AMD graphics device", "Dispositivo gráfico AMD", "Dispositivo gráfico AMD", "Συσκευή γραφικών AMD", "Dispositivo gráfico AMD"],
    Buffer => ["Audio buffer request (ms)", "Memória intermédia de áudio (ms)", "Buffer de áudio solicitado (ms)", "Προσωρινή μνήμη ήχου (ms)", "Búfer de audio solicitado (ms)"],
    Delay => ["Model delay: 60 ms, plus device and buffering delays.", "Atraso do modelo: 60 ms, mais os atrasos do dispositivo e da memória intermédia.", "Atraso do modelo: 60 ms, além dos atrasos do dispositivo e do buffer.", "Καθυστέρηση μοντέλου: 60 ms, συν τις καθυστερήσεις συσκευής και προσωρινής μνήμης.", "Retardo del modelo: 60 ms, más los retardos del dispositivo y del búfer."],
    Save => ["Save settings", "Guardar definições", "Salvar configurações", "Αποθήκευση ρυθμίσεων", "Guardar ajustes"],
    Automatic => ["Automatic selection", "Seleção automática", "Seleção automática", "Αυτόματη επιλογή", "Selección automática"],
    Peak => ["Peak", "Pico", "Pico", "Κορυφή", "Pico"],
    NoMeasured => ["No measured signal", "Sem sinal medido", "Sem sinal medido", "Δεν μετρήθηκε σήμα", "Sin señal medida"],
    PickerError => ["Could not select the model folder", "Não foi possível selecionar a pasta do modelo", "Não foi possível selecionar a pasta do modelo", "Δεν ήταν δυνατή η επιλογή του φακέλου μοντέλου", "No se pudo seleccionar la carpeta del modelo"],
    FolderTitle => ["Select model folder", "Selecionar pasta do modelo", "Selecionar pasta do modelo", "Επιλογή φακέλου μοντέλου", "Seleccionar carpeta del modelo"],
    Select => ["Select", "Selecionar", "Selecionar", "Επιλογή", "Seleccionar"],
    Cancel => ["Cancel", "Cancelar", "Cancelar", "Ακύρωση", "Cancelar"],
    SettingsSaved => ["Settings saved.", "Definições guardadas.", "Configurações salvas.", "Οι ρυθμίσεις αποθηκεύτηκαν.", "Ajustes guardados."],
    DefaultSelected => ["System microphone selected. Apps using a fixed input need their own selection.", "Microfone do sistema selecionado. As aplicações com uma entrada fixa precisam de uma seleção própria.", "Microfone do sistema selecionado. Os aplicativos com uma entrada fixa precisam de uma seleção própria.", "Επιλέχθηκε το μικρόφωνο συστήματος. Οι εφαρμογές με σταθερή είσοδο χρειάζονται ξεχωριστή επιλογή.", "Micrófono del sistema seleccionado. Las aplicaciones con una entrada fija necesitan su propia selección."],
    ChooseSource => ["Choose a microphone first", "Selecione primeiro um microfone", "Selecione um microfone primeiro", "Επιλέξτε πρώτα ένα μικρόφωνο", "Seleccione primero un micrófono"],
    ChooseBundle => ["Choose the model folder first", "Selecione primeiro a pasta do modelo", "Selecione primeiro a pasta do modelo", "Επιλέξτε πρώτα τον φάκελο του μοντέλου", "Seleccione primero la carpeta del modelo"],
    CannotSave => ["Cannot save settings", "Não foi possível guardar as definições", "Não foi possível salvar as configurações", "Αδυναμία αποθήκευσης ρυθμίσεων", "No se pueden guardar los ajustes"],
    StoppedError => ["Processing stopped. Check the source, model bundle, and AMD adapter.", "O processamento parou. Verifique a entrada, o modelo e o dispositivo AMD.", "O processamento parou. Verifique a entrada, o modelo e o dispositivo AMD.", "Η επεξεργασία σταμάτησε. Ελέγξτε την είσοδο, το μοντέλο και τη συσκευή AMD.", "El procesamiento se detuvo. Revise la entrada, el modelo y el dispositivo AMD."],
    LoadSettingsError => ["Could not load saved settings", "Não foi possível carregar as definições guardadas", "Não foi possível carregar as configurações salvas", "Αδυναμία φόρτωσης αποθηκευμένων ρυθμίσεων", "No se pudieron cargar los ajustes guardados"],
    PreservedSettings => ["The existing file is preserved until you explicitly save.", "O ficheiro existente será preservado até guardar explicitamente.", "O arquivo existente será preservado até você salvar explicitamente.", "Το υπάρχον αρχείο διατηρείται μέχρι να επιλέξετε αποθήκευση.", "El archivo existente se conserva hasta que guarde explícitamente."],
}
