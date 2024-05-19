use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use syn::{File as SynFile, Item, ItemStruct, Type, TypePath};

#[derive(Debug)]
enum RustSqlite {
    Integer,
    Float,
    Text,
    Boolean,
    Binary,
    Optional(Box<RustSqlite>),
    Other,
}

impl RustSqlite {
    fn from_syn_type(ty: &Type) -> Self {
        if let Type::Path(TypePath { path, .. }) = ty {
            let segment = path.segments.last().unwrap();
            let type_name = segment.ident.to_string();
            match type_name.as_str() {
                "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" | "isize"
                | "usize" => RustSqlite::Integer,
                "f32" | "f64" => RustSqlite::Float,
                "String" | "str" => RustSqlite::Text,
                "bool" => RustSqlite::Boolean,
                "Vec<u8>" => RustSqlite::Binary,
                "Option" => {
                    if let syn::PathArguments::AngleBracketed(ref args) =
                        segment.arguments
                    {
                        if let Some(syn::GenericArgument::Type(inner_type)) =
                            args.args.first()
                        {
                            return RustSqlite::Optional(Box::new(
                                RustSqlite::from_syn_type(inner_type),
                            ));
                        }
                    }
                    RustSqlite::Other
                },
                _ => RustSqlite::Other,
            }
        } else {
            RustSqlite::Other
        }
    }

    fn to_sql_type(&self) -> &str {
        match self {
            RustSqlite::Integer => "INTEGER",
            RustSqlite::Float => "REAL",
            RustSqlite::Text => "TEXT",
            RustSqlite::Boolean => "INTEGER",
            RustSqlite::Binary => "BLOB",
            RustSqlite::Optional(inner) => inner.to_sql_type(),
            RustSqlite::Other => "TEXT",
        }
    }
    fn nullability(&self) -> String {
        if matches!(self, RustSqlite::Optional(_)) {
            "".to_owned()
        } else {
            " NOT NULL".to_owned()
        }
    }
}

/// Read a Rust source file and parse it into a syn::File AST.
fn read_and_parse_file(file_path: &PathBuf) -> SynFile {
    let mut file = File::open(file_path).expect("Unable to open file");
    let mut content = String::new();
    file.read_to_string(&mut content)
        .expect("Unable to read file");
    syn::parse_file(&content).expect("Unable to parse file")
}

/// Extract structs from a syn::File.
fn extract_structs(parsed_file: &SynFile) -> Vec<&ItemStruct> {
    parsed_file
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Struct(item_struct) = item {
                Some(item_struct)
            } else {
                None
            }
        })
        .collect()
}

/// Convert a Rust struct to an SQL table definition.
fn struct_to_sql_table(item_struct: &ItemStruct) -> String {
    let fields: Vec<String> = item_struct
        .fields
        .iter()
        .filter_map(|field| {
            let field_name = field.ident.as_ref()?.to_string();
            let rust_type = RustSqlite::from_syn_type(&field.ty);
            Some(format!(
                "{} {}{}",
                field_name,
                rust_type.to_sql_type(),
                rust_type.nullability()
            ))
        })
        .collect();

    format!(
        "CREATE TABLE IF NOT EXISTS {}s (\n{}\n);",
        &item_struct.ident.to_string().to_lowercase(),
        fields.join(",\n")
    )
}

fn find_models_files(root: &Path) -> Vec<PathBuf> {
    let mut models_files = Vec::new();
    search_directory(root, &mut models_files);
    models_files
}

fn search_directory(dir: &Path, models_files: &mut Vec<PathBuf>) {
    // TODO: use a f.in iterator
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir).expect("Unable to read directory") {
            let entry = entry.expect("Unable to get directory entry");
            let path = entry.path();

            if path.is_dir() {
                search_directory(&path, models_files);
            } else if path.is_file() && path.file_name() == Some("models.rs".as_ref()) {
                models_files.push(path);
            }
        }
    }
}

pub fn makemigrations() {
    find_models_files(Path::new("src"))
        .iter()
        .map(|mf| read_and_parse_file(mf))
        .flat_map(|sf| {
            extract_structs(&sf)
                .iter()
                .map(|s| struct_to_sql_table(s))
                .collect::<Vec<_>>()
        })
        .for_each(|sql| println!("{}", sql));
}
