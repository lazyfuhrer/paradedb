use pgrx::*;
use tantivy::collector::TopDocs;

use crate::{index_access::utils::get_parade_index, parade_index::index::TantivyScanState};

#[pg_guard]
pub extern "C" fn ambeginscan(
    indexrel: pg_sys::Relation,
    nkeys: ::std::os::raw::c_int,
    norderbys: ::std::os::raw::c_int,
) -> pg_sys::IndexScanDesc {
    let mut scandesc: PgBox<pg_sys::IndexScanDescData> =
        unsafe { PgBox::from_pg(pg_sys::RelationGetIndexScan(indexrel, nkeys, norderbys)) };
    let index_relation = unsafe { PgRelation::from_pg(indexrel) };
    let index_name = index_relation.name().to_string();

    // Create the index and scan
    let parade_index = get_parade_index(index_name);
    let state = parade_index.scan();

    scandesc.opaque =
        PgMemoryContexts::CurrentMemoryContext.leak_and_drop_on_delete(state) as void_mut_ptr;
    scandesc.into_pg()
}

#[pg_guard]
pub extern "C" fn amrescan(
    scan: pg_sys::IndexScanDesc,
    keys: pg_sys::ScanKey,
    nkeys: ::std::os::raw::c_int,
    _orderbys: pg_sys::ScanKey,
    _norderbys: ::std::os::raw::c_int,
) {
    if nkeys == 0 {
        panic!("no ScanKeys provided");
    }

    let scan: PgBox<pg_sys::IndexScanDescData> = unsafe { PgBox::from_pg(scan) };

    let state =
        unsafe { (scan.opaque as *mut TantivyScanState).as_mut() }.expect("no scandesc state");
    let nkeys = nkeys as usize;
    let keys = unsafe { std::slice::from_raw_parts(keys as *const pg_sys::ScanKeyData, nkeys) };
    let query = unsafe {
        String::from_datum(keys[0].sk_argument, false).expect("failed to convert query to string")
    };

    let k: usize = 10000;
    let query_parser = &state.query_parser;
    let searcher = &state.searcher;

    let (tantivy_query, _) = query_parser.parse_query_lenient(&query);
    let top_docs = searcher
        .search(&tantivy_query, &TopDocs::with_limit(k))
        .unwrap();

    state.iterator =
        PgMemoryContexts::CurrentMemoryContext.leak_and_drop_on_delete(top_docs.into_iter());
}

#[pg_guard]
pub extern "C" fn amendscan(_scan: pg_sys::IndexScanDesc) {}

#[pg_guard]
pub extern "C" fn amgettuple(
    scan: pg_sys::IndexScanDesc,
    _direction: pg_sys::ScanDirection,
) -> bool {
    let mut scan: PgBox<pg_sys::IndexScanDescData> = unsafe { PgBox::from_pg(scan) };
    let state =
        unsafe { (scan.opaque as *mut TantivyScanState).as_mut() }.expect("no scandesc state");

    scan.xs_recheck = false;

    let iter = unsafe { state.iterator.as_mut() }.expect("no iterator in state");

    match iter.next() {
        Some((_score, doc_address)) => {
            #[cfg(any(feature = "pg10", feature = "pg11"))]
            let tid = &mut scan.xs_ctup.t_self;
            #[cfg(any(feature = "pg12", feature = "pg13", feature = "pg14", feature = "pg15"))]
            let tid = &mut scan.xs_heaptid;

            let searcher = &state.searcher;
            let schema = &state.schema;
            let retrieved_doc = searcher.doc(doc_address).unwrap();

            let heap_tid_field = schema
                .get_field("heap_tid")
                .expect("field 'heap_tid' not found in schema");

            if let tantivy::schema::Value::U64(heap_tid_value) = retrieved_doc
                .get_first(heap_tid_field)
                .expect("heap_tid field not found in doc")
            {
                u64_to_item_pointer(*heap_tid_value, tid);
            }

            if unsafe { !item_pointer_is_valid(tid) } {
                panic!("invalid item pointer: {:?}", item_pointer_get_both(*tid));
            }

            true
        }
        None => false,
    }
}

#[pg_guard]
pub extern "C" fn ambitmapscan(scan: pg_sys::IndexScanDesc, tbm: *mut pg_sys::TIDBitmap) -> i64 {
    let scan: PgBox<pg_sys::IndexScanDescData> = unsafe { PgBox::from_pg(scan) };
    let state =
        unsafe { (scan.opaque as *mut TantivyScanState).as_mut() }.expect("no scandesc state");
    let searcher = &state.searcher;
    let schema = &state.schema;

    let mut cnt = 0i64;
    let iterator = unsafe { state.iterator.as_mut() }.expect("no iterator in state");
    for (_score, doc_address) in iterator {
        let retrieved_doc = searcher.doc(doc_address).unwrap();
        let heap_tid_field = schema
            .get_field("heap_tid")
            .expect("field 'heap_tid' not found in schema");

        if let tantivy::schema::Value::U64(heap_tid_value) = retrieved_doc
            .get_first(heap_tid_field)
            .expect("heap_tid field not found in doc")
        {
            let mut tid = pg_sys::ItemPointerData::default();
            u64_to_item_pointer(*heap_tid_value, &mut tid);

            unsafe {
                pg_sys::tbm_add_tuples(tbm, &mut tid, 1, false);
            }

            cnt += 1;
        }
    }

    cnt
}
