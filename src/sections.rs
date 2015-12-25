use std::fmt;
use std::mem;
use std::raw;

use {P32, P64, ElfFile};
use header::{Header, Class};
use parsing::{parse_one, parse_array, parse_str_array, parse_str};
use symbol_table;
use dynamic::Dynamic;
use hash::HashTable;

pub fn parse_section_header<'a>(input: &'a [u8],
                                header: Header<'a>,
                                index: u16) -> SectionHeader<'a> {
    // Trying to get index 0 (SHN_UNDEF) is also probably an error, but it is a legitimate section.
    assert!(index < SHN_LORESERVE, "Attempt to get section for a reserved index");

    let start = (index as u64 * header.pt2.sh_entry_size() as u64 + header.pt2.sh_offset() as u64) as usize;
    let end = start + header.pt2.sh_entry_size() as usize;

    match header.pt1.class {
        Class::ThirtyTwo => {
            let header: &'a SectionHeader_<P32> = parse_one(&input[start..end]);
            SectionHeader::Sh32(header)
        }
        Class::SixtyFour => {
            let header: &'a SectionHeader_<P64> = parse_one(&input[start..end]);
            SectionHeader::Sh64(header)
        }
        Class::None => unreachable!(),
    }
}

pub struct SectionIter<'b, 'a: 'b> {
    pub file: &'b ElfFile<'a>,
    pub next_index: u16,
}

impl<'b, 'a> Iterator for SectionIter<'b, 'a> {
    type Item = SectionHeader<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let count = self.file.header.pt2.sh_count();
        if self.next_index >= count {
            return None;
        }

        let result = Some(parse_section_header(&self.file.input,
                                               self.file.header,
                                               self.next_index));
        self.next_index += 1;
        result
    }
}

// Distinguished section indices.
pub const SHN_UNDEF: u16        = 0;
pub const SHN_LORESERVE: u16    = 0xff00;
pub const SHN_LOPROC: u16       = 0xff00;
pub const SHN_HIPROC: u16       = 0xff1f;
pub const SHN_LOOS: u16         = 0xff20;
pub const SHN_HIOS: u16         = 0xff3f;
pub const SHN_ABS: u16          = 0xfff1;
pub const SHN_COMMON: u16       = 0xfff2;
pub const SHN_XINDEX: u16       = 0xffff;
pub const SHN_HIRESERVE: u16    = 0xffff;

#[derive(Clone, Copy)]
pub enum SectionHeader<'a> {
    Sh32(&'a SectionHeader_<P32>),
    Sh64(&'a SectionHeader_<P64>),
}

macro_rules! getter {
    ($name: ident, $typ: ident) => {
        pub fn $name(&self) -> $typ {
            match *self {
                SectionHeader::Sh32(h) => h.$name as $typ,
                SectionHeader::Sh64(h) => h.$name as $typ,
            }        
        }
    }
}

impl<'a> SectionHeader<'a> {
    // Note that this function is O(n) in the length of the name.
    pub fn get_name(&self, elf_file: &ElfFile<'a>) -> Result<&'a str, &'static str> {
        if self.get_type() == ShType::Null {
            return Err("Attempt to get name of null section");
        }

        Ok(elf_file.get_string(self.name()))
    }

    pub fn get_type(&self) -> ShType {
        self.type_().as_sh_type()
    }

    pub fn get_data(&self, elf_file: &ElfFile<'a>) -> SectionData<'a> {
        type Dynamic32 = Dynamic<P32>;
        type Dynamic64 = Dynamic<P64>;
        macro_rules! array_data {
            ($data32: ident, $data64: ident) => {{
                let data = self.raw_data(elf_file);
                match elf_file.header.pt1.class {
                    Class::ThirtyTwo => SectionData::$data32(parse_array(data)),
                    Class::SixtyFour => SectionData::$data64(parse_array(data)),
                    Class::None => unreachable!(),
                }
            }}
        }

        match self.get_type() {
            ShType::Null | ShType::NoBits => SectionData::Empty,
            ShType::ProgBits | ShType::ShLib | ShType::OsSpecific(_) |
            ShType::ProcessorSpecific(_) | ShType::User(_) => {
                SectionData::Undefined(self.raw_data(elf_file))
            }
            ShType::SymTab | ShType::DynSym => {
                array_data!(SymbolTable32, SymbolTable64)
            }
            ShType::StrTab => SectionData::StrArray(self.raw_data(elf_file)),
            ShType::InitArray | ShType::FiniArray | ShType::PreInitArray => {
                array_data!(FnArray32, FnArray64)
            }
            ShType::Rela => {
                array_data!(Rela32, Rela64)
            }
            ShType::Rel => {
                array_data!(Rel32, Rel64)
            }
            ShType::Dynamic => {
                array_data!(Dynamic32, Dynamic64)                
            }
            ShType::Group => {
                let data = self.raw_data(elf_file);
                unsafe {
                    let flags: &'a u32 = mem::transmute(&data[0]);
                    let indicies: &'a [u32] = parse_array(&data[4..]);
                    SectionData::Group { flags: flags, indicies: indicies }
                }
            }
            ShType::SymTabShIndex => {
                SectionData::SymTabShIndex(parse_array(self.raw_data(elf_file)))
            }
            ShType::Note => {
                let data = self.raw_data(elf_file);
                match elf_file.header.pt1.class {
                    Class::ThirtyTwo => unimplemented!(),
                    Class::SixtyFour => {
                        let header: &'a NoteHeader = parse_one(&data[0..3]);
                        let index = &data[3];
                        SectionData::Note64(header, index)
                    }
                    Class::None => unreachable!(),
                }

            }
            ShType::Hash => {
                let data = self.raw_data(elf_file);
                SectionData::HashTable(parse_one(&data[0..12]))
            }
        }
    }

    pub fn raw_data(&self, elf_file: &ElfFile<'a>) -> &'a [u8] {
        assert!(self.get_type() != ShType::Null);
        &elf_file.input[self.offset() as usize..(self.offset() + self.size()) as usize]
    }

    getter!(name, u32);
    getter!(offset, u64);
    getter!(size, u64);
    getter!(type_, ShType_);
}

impl<'a> fmt::Display for SectionHeader<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            SectionHeader::Sh32(sh) => sh.fmt(f),
            SectionHeader::Sh64(sh) => sh.fmt(f),
        }
    }
}

impl<P: fmt::Debug> fmt::Display for SectionHeader_<P> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        try!(writeln!(f, "Section header:"));
        try!(writeln!(f, "    name:             {:?}", self.name));
        try!(writeln!(f, "    type:             {:?}", self.type_));
        try!(writeln!(f, "    flags:            {:?}", self.flags));
        try!(writeln!(f, "    address:          {:?}", self.address));
        try!(writeln!(f, "    offset:           {:?}", self.offset));
        try!(writeln!(f, "    size:             {:?}", self.size));
        try!(writeln!(f, "    link:             {:?}", self.link));
        try!(writeln!(f, "    align:            {:?}", self.align));
        try!(writeln!(f, "    entry size:       {:?}", self.entry_size));
        Ok(())
    }
}

#[derive(Debug)]
#[repr(C)]
pub struct SectionHeader_<P> {
    name: u32,
    type_: ShType_,
    flags: P,
    address: P,
    offset: P,
    size: P,
    link: u32,
    info: u32,
    align: P,
    entry_size: P,
}

#[derive(Copy, Clone)]
pub struct ShType_(u32);

#[derive(Debug, PartialEq, Eq)]
pub enum ShType {
    Null,
    ProgBits,
    SymTab,
    StrTab,
    Rela,
    Hash,
    Dynamic,
    Note,
    NoBits,
    Rel,
    ShLib,
    DynSym,
    InitArray,
    FiniArray,
    PreInitArray,
    Group,
    SymTabShIndex,
    OsSpecific(u32),
    ProcessorSpecific(u32),
    User(u32),
}

impl ShType_ {
    fn as_sh_type(self) -> ShType {
        match self.0 {
            0 => ShType::Null,
            1 => ShType::ProgBits,
            2 => ShType::SymTab,
            3 => ShType::StrTab,
            4 => ShType::Rela,
            5 => ShType::Hash,
            6 => ShType::Dynamic,
            7 => ShType::Note,
            8 => ShType::NoBits,
            9 => ShType::Rel,
            10 => ShType::ShLib,
            11 => ShType::DynSym,
            // sic.
            14 => ShType::InitArray,
            15 => ShType::FiniArray,
            16 => ShType::PreInitArray,
            17 => ShType::Group,
            18 => ShType::SymTabShIndex,
            st if st >= SHT_LOOS && st <= SHT_HIOS => ShType::OsSpecific(st),
            st if st >= SHT_LOPROC && st <= SHT_HIPROC => ShType::ProcessorSpecific(st),
            st if st >= SHT_LOUSER && st <= SHT_HIUSER => ShType::User(st),
            _ => panic!("Invalid sh type"),
        }
    }
}

impl fmt::Debug for ShType_ {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.as_sh_type().fmt(f)
    }
}

pub enum SectionData<'a> {
    Empty,
    Undefined(&'a [u8]),
    Group { flags: &'a u32, indicies: &'a[u32] },
    StrArray(&'a [u8]),
    FnArray32(&'a [u32]),
    FnArray64(&'a [u64]),
    SymbolTable32(&'a [symbol_table::Entry32]),
    SymbolTable64(&'a [symbol_table::Entry64]),
    SymTabShIndex(&'a [u32]),
    // Note32 uses 4-byte words, which I'm not sure how to manage.
    // The pointer is to the start of the name field in the note.
    Note64(&'a NoteHeader, &'a u8),
    Rela32(&'a [Rela<P32>]),
    Rela64(&'a [Rela<P64>]),
    Rel32(&'a [Rel<P32>]),
    Rel64(&'a [Rel<P64>]),
    Dynamic32(&'a [Dynamic<P64>]),
    Dynamic64(&'a [Dynamic<P32>]),
    HashTable(&'a HashTable),
}

impl<'a> SectionData<'a> {
    // Allocates a Vec for the pointers (but not strings). O(n) in the size of
    // the string table.
    pub fn to_strings(&self) -> Result<Vec<&'a str>, ()> {
        if let SectionData::StrArray(data) = *self {
            Ok(parse_str_array(data))
        } else {
            Err(())
        }
    }
}

// Distinguished ShType values.
pub const SHT_LOOS: u32   = 0x60000000;
pub const SHT_HIOS: u32   = 0x6fffffff;
pub const SHT_LOPROC: u32 = 0x70000000;
pub const SHT_HIPROC: u32 = 0x7fffffff;
pub const SHT_LOUSER: u32 = 0x80000000;
pub const SHT_HIUSER: u32 = 0xffffffff;

// Flags (SectionHeader::flags)
pub const SHF_WRITE: u64            =        0x1;
pub const SHF_ALLOC: u64            =        0x2;
pub const SHF_EXECINSTR: u64        =        0x4;
pub const SHF_MERGE: u64            =       0x10;
pub const SHF_STRINGS: u64          =       0x20;
pub const SHF_INFO_LINK: u64        =       0x40;
pub const SHF_LINK_ORDER: u64       =       0x80;
pub const SHF_OS_NONCONFORMING: u64 =      0x100;
pub const SHF_GROUP: u64            =      0x200;
pub const SHF_TLS: u64              =      0x400;
pub const SHF_COMPRESSED: u64       =      0x800;
pub const SHF_MASKOS: u64           = 0x0ff00000;
pub const SHF_MASKPROC: u64         = 0xf0000000;

#[derive(Debug)]
#[repr(C)]
pub struct CompressionHeader64 {
    type_: CompressionType_,
    _reserved: u32,
    size: u64,
    align: u64,
}

#[derive(Debug)]
#[repr(C)]
pub struct CompressionHeader32 {
    type_: CompressionType_,
    size: u32,
    align: u32,
}

pub struct CompressionType_(u32);

#[derive(Debug)]
pub enum CompressionType {
    Zlib,
    OsSpecific(u32),
    ProcessorSpecific(u32),
}

impl CompressionType_ {
    fn as_compression_type(&self) -> CompressionType {
        match self.0 {
            1 => CompressionType::Zlib,
            st if st >= COMPRESS_LOOS && st <= COMPRESS_HIOS => CompressionType::OsSpecific(st),
            st if st >= COMPRESS_LOPROC && st <= COMPRESS_HIPROC => CompressionType::ProcessorSpecific(st),
            _ => panic!("Invalid sh type"),
        }
    }
}

impl fmt::Debug for CompressionType_ {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.as_compression_type().fmt(f)
    }
}

// Distinguished CompressionType values.
pub const COMPRESS_LOOS: u32   = 0x60000000;
pub const COMPRESS_HIOS: u32   = 0x6fffffff;
pub const COMPRESS_LOPROC: u32 = 0x70000000;
pub const COMPRESS_HIPROC: u32 = 0x7fffffff;

// Group flags
pub const GRP_COMDAT: u64   =        0x1;
pub const GRP_MASKOS: u64   = 0x0ff00000;
pub const GRP_MASKPROC: u64 = 0xf0000000;

pub struct Rela<P> {
    offset: P,
    info: P,
    addend: P,
}

pub struct Rel<P> {
    offset: P,
    info: P,    
}

impl Rela<P32> {
    pub fn get_symbol_table_index(&self) -> u32 {
        self.info >> 8
    }
    pub fn get_type(&self) -> u8 {
        self.info as u8
    }
}
impl Rela<P64> {
    pub fn get_symbol_table_index(&self) -> u32 {
        (self.info >> 32) as u32
    }
    pub fn get_type(&self) -> u32 {
        (self.info & 0xffffffff) as u32
    }
}
impl Rel<P32> {
    pub fn get_symbol_table_index(&self) -> u32 {
        self.info >> 8
    }
    pub fn get_type(&self) -> u8 {
        self.info as u8
    }
}
impl Rel<P64> {
    pub fn get_symbol_table_index(&self) -> u32 {
        (self.info >> 32) as u32
    }
    pub fn get_type(&self) -> u32 {
        (self.info & 0xffffffff) as u32
    }
}
#[derive(Debug)]
#[repr(C)]
pub struct NoteHeader {
    name_size: u32,
    desc_size: u32,
    type_: u32,
}

impl NoteHeader {
    fn name<'a>(&'a self, name_index: &'a u8) -> &'a str {
        let result = parse_str(name_index, 0);
        // - 1 is due to null terminator
        assert!(result.len() == (self.name_size - 1) as usize);
        result
    }

    fn desc<'a>(&'a self, name_index: &'a u8) -> &'a [u8] {
        // Account for padding to the next u32.
        unsafe {
            let offset = (self.name_size + 3) & !0x3;
            let ptr = (name_index as *const u8).offset(offset as isize);
            let slice = raw::Slice { data: ptr, len: self.desc_size as usize };
            mem::transmute(slice)
        }
    }
}

pub fn sanity_check<'a>(header: SectionHeader<'a>, file: &ElfFile<'a>) -> Result<(), &'static str> {
    if header.get_type() == ShType::Null {
        return Ok(());
    }
    // TODO
    Ok(())
}