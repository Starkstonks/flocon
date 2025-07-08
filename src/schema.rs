// @generated automatically by Diesel CLI.

diesel::table! {
    block (id) {
        id -> Integer,
        inode_id -> Integer,
        first_byte -> Integer,
        last_byte -> Integer,
        data -> Nullable<Binary>,
    }
}

diesel::table! {
    inode (id) {
        id -> Integer,
        mode -> Integer,
        uid -> Integer,
        gid -> Integer,
        size -> Integer,
        atime -> Text,
        mtime -> Text,
        ctime -> Text,
        btime -> Text,
    }
}

diesel::table! {
    link (parent_id, name) {
        parent_id -> Integer,
        child_id -> Integer,
        name -> Text,
    }
}

diesel::table! {
    seaql_migrations (version) {
        version -> Text,
        applied_at -> BigInt,
    }
}

diesel::table! {
    xattr (inode_id, name) {
        inode_id -> Integer,
        name -> Text,
        value -> Binary,
    }
}

diesel::joinable!(block -> inode (inode_id));
diesel::joinable!(xattr -> inode (inode_id));

diesel::allow_tables_to_appear_in_same_query!(
    block,
    inode,
    link,
    seaql_migrations,
    xattr,
);
