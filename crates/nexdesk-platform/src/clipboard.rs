use crate::InputError;

pub struct Clipboard(arboard::Clipboard);

impl Clipboard {
    pub fn open() -> Result<Self, InputError> {
        arboard::Clipboard::new()
            .map(Self)
            .map_err(|_| InputError::Unavailable)
    }

    pub fn read(&mut self) -> Result<String, InputError> {
        match self.0.get_text() {
            Ok(value) => Ok(value),
            Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
            Err(_) => Err(InputError::Unavailable),
        }
    }

    pub fn write(&mut self, text: String) -> Result<(), InputError> {
        self.0.set_text(text).map_err(|_| InputError::Rejected)
    }
}
