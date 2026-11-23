//! Small owned SQLite interface. SQL is static; every external value is bound.
use crate::util::Result;
use serde_json::{Map, Number, Value};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::Path,
    ptr,
};
#[repr(C)]
struct Sqlite {
    _private: [u8; 0],
}
#[repr(C)]
struct Stmt {
    _private: [u8; 0],
}
#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open(filename: *const c_char, db: *mut *mut Sqlite) -> c_int;
    fn sqlite3_close(db: *mut Sqlite) -> c_int;
    fn sqlite3_errmsg(db: *mut Sqlite) -> *const c_char;
    fn sqlite3_busy_timeout(db: *mut Sqlite, ms: c_int) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite,
        sql: *const c_char,
        n: c_int,
        stmt: *mut *mut Stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_finalize(stmt: *mut Stmt) -> c_int;
    fn sqlite3_step(stmt: *mut Stmt) -> c_int;
    fn sqlite3_bind_null(stmt: *mut Stmt, i: c_int) -> c_int;
    fn sqlite3_bind_double(stmt: *mut Stmt, i: c_int, v: f64) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut Stmt,
        i: c_int,
        v: *const c_char,
        n: c_int,
        d: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    fn sqlite3_column_count(stmt: *mut Stmt) -> c_int;
    fn sqlite3_column_name(stmt: *mut Stmt, i: c_int) -> *const c_char;
    fn sqlite3_column_type(stmt: *mut Stmt, i: c_int) -> c_int;
    fn sqlite3_column_text(stmt: *mut Stmt, i: c_int) -> *const u8;
    fn sqlite3_column_bytes(stmt: *mut Stmt, i: c_int) -> c_int;
    fn sqlite3_column_double(stmt: *mut Stmt, i: c_int) -> f64;
    fn sqlite3_changes(db: *mut Sqlite) -> c_int;
}
pub struct Database(*mut Sqlite);
struct Statement(*mut Stmt);
impl Drop for Statement {
    fn drop(&mut self) {
        unsafe {
            sqlite3_finalize(self.0);
        }
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        unsafe {
            sqlite3_close(self.0);
        }
    }
}
impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_encoded_bytes())?;
        let mut raw = ptr::null_mut();
        let code = unsafe { sqlite3_open(path.as_ptr(), &mut raw) };
        if raw.is_null() {
            return Err("Could not open reminders database".into());
        }
        let db = Self(raw);
        db.check(code)?;
        db.check(unsafe { sqlite3_busy_timeout(raw, 3000) })?;
        Ok(db)
    }
    fn check(&self, code: i32) -> Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(unsafe { CStr::from_ptr(sqlite3_errmsg(self.0)) }
                .to_string_lossy()
                .into_owned()
                .into())
        }
    }
    pub fn query(&self, sql: &str, args: &[Value]) -> Result<Vec<Value>> {
        // Declared before the statement so these allocations also outlive its
        // finalizer on every error path (bindings use SQLITE_STATIC).
        let strings: Vec<Option<CString>> = args
            .iter()
            .map(|v| v.as_str().map(CString::new).transpose())
            .collect::<std::result::Result<_, _>>()?;
        let sql = CString::new(sql)?;
        let mut raw = ptr::null_mut();
        let code =
            unsafe { sqlite3_prepare_v2(self.0, sql.as_ptr(), -1, &mut raw, ptr::null_mut()) };
        let stmt = Statement(raw);
        self.check(code)?;
        for (index, arg) in args.iter().enumerate() {
            let i = index as c_int + 1;
            let code = unsafe {
                if let Some(text) = &strings[index] {
                    sqlite3_bind_text(
                        stmt.0,
                        i,
                        text.as_ptr(),
                        text.as_bytes().len().try_into()?,
                        None,
                    )
                } else if let Some(number) = arg.as_f64() {
                    sqlite3_bind_double(stmt.0, i, number)
                } else {
                    sqlite3_bind_null(stmt.0, i)
                }
            };
            self.check(code)?;
        }
        let mut rows = Vec::new();
        loop {
            let code = unsafe { sqlite3_step(stmt.0) };
            if code == 101 {
                break;
            }
            if code != 100 {
                self.check(code)?;
            }
            let mut row = Map::new();
            for i in 0..unsafe { sqlite3_column_count(stmt.0) } {
                let name = unsafe { CStr::from_ptr(sqlite3_column_name(stmt.0, i)) }
                    .to_string_lossy()
                    .into_owned();
                let value = unsafe {
                    match sqlite3_column_type(stmt.0, i) {
                        1 | 2 => Number::from_f64(sqlite3_column_double(stmt.0, i))
                            .map(Value::Number)
                            .unwrap_or(Value::Null),
                        3 => {
                            let p = sqlite3_column_text(stmt.0, i);
                            let n = sqlite3_column_bytes(stmt.0, i);
                            if p.is_null() {
                                return Err("SQLite could not allocate text".into());
                            }
                            Value::String(
                                String::from_utf8_lossy(std::slice::from_raw_parts(p, n as usize))
                                    .into_owned(),
                            )
                        }
                        _ => Value::Null,
                    }
                };
                row.insert(name, value);
            }
            rows.push(Value::Object(row));
        }
        Ok(rows)
    }
    pub fn execute(&self, sql: &str, args: &[Value]) -> Result<usize> {
        self.query(sql, args)?;
        Ok(unsafe { sqlite3_changes(self.0) } as usize)
    }
    pub fn transaction<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        self.execute("BEGIN IMMEDIATE", &[])?;
        match f(self) {
            Ok(v) => {
                self.execute("COMMIT", &[])?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.execute("ROLLBACK", &[]);
                Err(e)
            }
        }
    }
}
