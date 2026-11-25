//! `clipboard`: text, images and copied files. Text goes through each
//! system's own backend (`macos/clipboard.rs`, `linux/clipboard.rs`,
//! `windows/clipboard.rs`). Images and files go through a [`ClipboardData`]
//! (arboard on all three, [`ArboardData`]); [`Rich`] adds them to a text
//! backend and turns images into PNG files in the caller's data folder,
//! since a path is what an agent or a plugin can use next.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;

#[async_trait]
pub trait ClipboardProvider: Send + Sync {
    async fn get(&self, caller: &Caller) -> Result<Option<String>, CallError>;
    async fn set(&self, caller: &Caller, text: String) -> Result<(), CallError>;

    /// Among `text`, `image` and `files`.
    async fn formats(&self, _caller: &Caller) -> Result<Vec<String>, CallError> {
        Err(CallError::Unavailable)
    }
    /// The path of a PNG of the clipboard's image.
    async fn get_image(&self, _caller: &Caller) -> Result<Option<String>, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_image(&self, _caller: &Caller, _path: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn get_files(&self, _caller: &Caller) -> Result<Vec<String>, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_files(&self, _caller: &Caller, _paths: Vec<String>) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// No clipboard on this node.
pub struct NoClipboard;

#[async_trait]
impl ClipboardProvider for NoClipboard {
    async fn get(&self, _: &Caller) -> Result<Option<String>, CallError> {
        Err(CallError::Unavailable)
    }

    async fn set(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// An image as RGBA rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The non-text contents of one system's clipboard. Calls block.
pub trait ClipboardData: Send + Sync + 'static {
    fn has_text(&self) -> Result<bool, CallError>;
    fn image(&self) -> Result<Option<Image>, CallError>;
    fn set_image(&self, image: Image) -> Result<(), CallError>;
    fn files(&self) -> Result<Vec<PathBuf>, CallError>;
    fn set_files(&self, files: &[PathBuf]) -> Result<(), CallError>;
}

/// A text clipboard with images and files.
pub struct Rich {
    text: Arc<dyn ClipboardProvider>,
    data: Arc<dyn ClipboardData>,
    /// Holds `<plugin>/clipboard/` for each caller.
    data_dir: PathBuf,
    home: PathBuf,
}

impl Rich {
    pub fn new(
        text: Arc<dyn ClipboardProvider>,
        data: Arc<dyn ClipboardData>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            text,
            data,
            data_dir,
            home: crate::disk::home(),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn ClipboardData) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        let data = self.data.clone();
        tokio::task::spawn_blocking(move || f(&*data))
            .await
            .map_err(CallError::failed)?
    }

    /// The file, if it is in the caller's disk grants or its data folder.
    fn readable(&self, caller: &Caller, path: &str) -> Result<PathBuf, CallError> {
        let own = self.data_dir.join(&caller.plugin);
        let mut grants = caller.disk.clone();
        grants.push(crate::disk::DiskGrant {
            root: own,
            write: false,
        });
        crate::disk::authorize(path, &grants, &self.home, false)
    }
}

/// PNG bytes of `image`.
pub fn encode_png(image: &Image) -> Result<Vec<u8>, CallError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, image.width, image.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(CallError::failed)?;
        w.write_image_data(&image.rgba).map_err(CallError::failed)?;
    }
    Ok(out)
}

/// Decodes a PNG or JPEG file.
pub fn decode_image(bytes: &[u8]) -> Result<Image, CallError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| CallError::Failed(format!("not a PNG or JPEG image: {e}")))?
        .into_rgba8();
    Ok(Image {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    })
}

fn save(dir: &Path, bytes: &[u8]) -> Result<PathBuf, CallError> {
    std::fs::create_dir_all(dir).map_err(CallError::failed)?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{ms}.png"));
    std::fs::write(&path, bytes).map_err(CallError::failed)?;
    Ok(path)
}

#[async_trait]
impl ClipboardProvider for Rich {
    async fn get(&self, caller: &Caller) -> Result<Option<String>, CallError> {
        self.text.get(caller).await
    }

    async fn set(&self, caller: &Caller, text: String) -> Result<(), CallError> {
        self.text.set(caller, text).await
    }

    async fn formats(&self, _: &Caller) -> Result<Vec<String>, CallError> {
        self.blocking(|d| {
            let mut out = Vec::new();
            if d.has_text()? {
                out.push("text".to_string());
            }
            if d.image()?.is_some() {
                out.push("image".to_string());
            }
            if !d.files()?.is_empty() {
                out.push("files".to_string());
            }
            Ok(out)
        })
        .await
    }

    async fn get_image(&self, caller: &Caller) -> Result<Option<String>, CallError> {
        let dir = self.data_dir.join(&caller.plugin).join("clipboard");
        self.blocking(move |d| {
            let Some(image) = d.image()? else {
                return Ok(None);
            };
            let path = save(&dir, &encode_png(&image)?)?;
            Ok(Some(path.display().to_string()))
        })
        .await
    }

    async fn set_image(&self, caller: &Caller, path: String) -> Result<(), CallError> {
        let real = self.readable(caller, &path)?;
        self.blocking(move |d| {
            let bytes = std::fs::read(&real)
                .map_err(|e| CallError::Failed(format!("cannot read {}: {e}", real.display())))?;
            d.set_image(decode_image(&bytes)?)
        })
        .await
    }

    async fn get_files(&self, _: &Caller) -> Result<Vec<String>, CallError> {
        self.blocking(|d| {
            Ok(d.files()?
                .into_iter()
                .map(|p| p.display().to_string())
                .collect())
        })
        .await
    }

    async fn set_files(&self, caller: &Caller, paths: Vec<String>) -> Result<(), CallError> {
        if paths.is_empty() {
            return Err(CallError::Failed("invalid_argument: no files".into()));
        }
        // Copying a path does not read it, but a paste can: keep to what
        // the caller may read.
        let files = paths
            .iter()
            .map(|p| self.readable(caller, p))
            .collect::<Result<Vec<_>, _>>()?;
        for f in &files {
            if !f.exists() {
                return Err(CallError::Failed(format!("no file {}", f.display())));
            }
        }
        self.blocking(move |d| d.set_files(&files)).await
    }
}

/// Images and files through arboard, on one thread that keeps the
/// clipboard open: on X11 what we set lives only while its owner does.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub struct ArboardData {
    tx: std::sync::Mutex<Option<std::sync::mpsc::Sender<Job>>>,
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
type Job = Box<dyn FnOnce(Result<&mut arboard::Clipboard, CallError>) + Send>;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl Default for ArboardData {
    fn default() -> Self {
        Self {
            tx: std::sync::Mutex::new(None),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl ArboardData {
    fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        let (done, wait) = std::sync::mpsc::channel();
        let job: Job = Box::new(move |board| {
            let _ = done.send(board.and_then(f));
        });
        {
            let mut slot = self.tx.lock().unwrap();
            let tx = slot.get_or_insert_with(start_arboard);
            if let Err(std::sync::mpsc::SendError(job)) = tx.send(job) {
                let fresh = start_arboard();
                fresh.send(job).map_err(|_| CallError::Unavailable)?;
                *slot = Some(fresh);
            }
        }
        wait.recv().map_err(|_| CallError::Unavailable)?
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn start_arboard() -> std::sync::mpsc::Sender<Job> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let _ = std::thread::Builder::new()
        .name("bricolo-clipboard-data".into())
        .spawn(move || {
            let mut board: Option<arboard::Clipboard> = None;
            for job in rx {
                if board.is_none() {
                    board = arboard::Clipboard::new()
                        .map_err(|e| tracing::debug!("clipboard: cannot open: {e}"))
                        .ok();
                }
                match board.as_mut() {
                    Some(b) => job(Ok(b)),
                    None => job(Err(CallError::Unavailable)),
                }
            }
        });
    tx
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn absent<T>(r: Result<T, arboard::Error>) -> Result<Option<T>, CallError> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(e) => Err(CallError::failed(e)),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
impl ClipboardData for ArboardData {
    fn has_text(&self) -> Result<bool, CallError> {
        self.run(|b| Ok(absent(b.get_text())?.is_some_and(|t| !t.is_empty())))
    }

    fn image(&self) -> Result<Option<Image>, CallError> {
        self.run(|b| {
            Ok(absent(b.get_image())?.map(|i| Image {
                width: i.width as u32,
                height: i.height as u32,
                rgba: i.bytes.into_owned(),
            }))
        })
    }

    fn set_image(&self, image: Image) -> Result<(), CallError> {
        self.run(move |b| {
            b.set_image(arboard::ImageData {
                width: image.width as usize,
                height: image.height as usize,
                bytes: image.rgba.into(),
            })
            .map_err(CallError::failed)
        })
    }

    fn files(&self) -> Result<Vec<PathBuf>, CallError> {
        self.run(|b| Ok(absent(b.get().file_list())?.unwrap_or_default()))
    }

    fn set_files(&self, files: &[PathBuf]) -> Result<(), CallError> {
        let files = files.to_vec();
        self.run(move |b| b.set().file_list(&files).map_err(CallError::failed))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::disk::DiskGrant;

    #[derive(Default)]
    struct Fake {
        image: Mutex<Option<Image>>,
        files: Mutex<Vec<PathBuf>>,
    }

    impl ClipboardData for Fake {
        fn has_text(&self) -> Result<bool, CallError> {
            Ok(false)
        }
        fn image(&self) -> Result<Option<Image>, CallError> {
            Ok(self.image.lock().unwrap().clone())
        }
        fn set_image(&self, image: Image) -> Result<(), CallError> {
            *self.image.lock().unwrap() = Some(image);
            Ok(())
        }
        fn files(&self) -> Result<Vec<PathBuf>, CallError> {
            Ok(self.files.lock().unwrap().clone())
        }
        fn set_files(&self, files: &[PathBuf]) -> Result<(), CallError> {
            *self.files.lock().unwrap() = files.to_vec();
            Ok(())
        }
    }

    fn rich(dir: &Path) -> (Rich, Arc<Fake>) {
        let fake = Arc::new(Fake::default());
        (
            Rich::new(Arc::new(NoClipboard), fake.clone(), dir.join("data")),
            fake,
        )
    }

    #[tokio::test]
    async fn images_round_trip_through_png_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (r, fake) = rich(&root);
        let c = Caller::new("p");
        assert_eq!(r.get_image(&c).await.unwrap(), None);
        assert!(r.formats(&c).await.unwrap().is_empty());
        let red = Image {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        *fake.image.lock().unwrap() = Some(red.clone());
        assert_eq!(r.formats(&c).await.unwrap(), ["image"]);
        let path = r.get_image(&c).await.unwrap().unwrap();
        assert!(path.starts_with(&root.join("data/p/clipboard").display().to_string()));
        // The caller's own data folder is readable without a grant.
        *fake.image.lock().unwrap() = None;
        r.set_image(&c, path.clone()).await.unwrap();
        assert_eq!(*fake.image.lock().unwrap(), Some(red));
        // Another caller's folder is not.
        assert_eq!(
            r.set_image(&Caller::new("q"), path).await,
            Err(CallError::Denied)
        );
        let txt = root.join("note.txt");
        std::fs::write(&txt, "hi").unwrap();
        let granted = Caller {
            plugin: "p".into(),
            disk: vec![DiskGrant {
                root: root.clone(),
                write: false,
            }],
            ..Caller::default()
        };
        let e = r
            .set_image(&granted, txt.display().to_string())
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("not a PNG")));
    }

    #[tokio::test]
    async fn files_need_read_grants_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (r, fake) = rich(&root);
        let a = root.join("a.txt");
        std::fs::write(&a, "a").unwrap();
        let c = Caller {
            plugin: "p".into(),
            disk: vec![DiskGrant {
                root: root.clone(),
                write: false,
            }],
            ..Caller::default()
        };
        r.set_files(&c, vec![a.display().to_string()])
            .await
            .unwrap();
        assert_eq!(*fake.files.lock().unwrap(), std::slice::from_ref(&a));
        assert_eq!(r.get_files(&c).await.unwrap(), [a.display().to_string()]);
        assert_eq!(r.formats(&c).await.unwrap(), ["files"]);
        assert_eq!(
            r.set_files(&Caller::new("p"), vec![a.display().to_string()])
                .await,
            Err(CallError::Denied)
        );
        assert!(
            r.set_files(&c, vec![root.join("missing").display().to_string()])
                .await
                .is_err()
        );
        assert!(r.set_files(&c, vec![]).await.is_err());
    }

    #[test]
    fn png_encoding_decodes_back() {
        let img = Image {
            width: 1,
            height: 2,
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 128],
        };
        assert_eq!(decode_image(&encode_png(&img).unwrap()).unwrap(), img);
    }
}
