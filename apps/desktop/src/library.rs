pub use rebook_app_core::library::*;

pub fn load_default() -> LibraryResult<LocalLibrary> {
    let project = crate::smoke::project_dirs().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "无法确定本地书架数据目录"))?;
    LocalLibrary::load_from(project.data_local_dir().join("library"))
}
