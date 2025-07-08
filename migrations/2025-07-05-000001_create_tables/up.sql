-- Main inode table to store file/directory metadata
create table inode (
    id integer primary key,
    mode integer not null,           -- File type and permissions
    uid integer not null,            -- User ID
    gid integer not null,            -- Group ID
    size integer not null default 0, -- File size in bytes
    atime datetime not null,         -- Access time
    mtime datetime not null,         -- Modification time
    ctime datetime not null,         -- Change time
    btime datetime not null          -- Birth/creation time
);

-- Directory structure - links between parent and child inodes
create table link (
    parent_id integer not null,
    child_id integer not null,
    name text not null,

    primary key (parent_id, name),
    foreign key (parent_id) references inode(id) on delete cascade,
    foreign key (child_id) references inode(id) on delete cascade
);

-- File data storage in blocks
create table block (
    id integer primary key,
    inode_id integer not null,
    first_byte integer not null,    -- Starting byte position (inclusive)
    last_byte integer not null,     -- Ending byte position (inclusive)
    data blob,                      -- Actual data (null for zero-filled blocks)

    foreign key (inode_id) references inode(id) on delete cascade,
    check (first_byte <= last_byte)
);

-- Extended attributes storage
create table xattr (
    inode_id integer not null,
    name text not null,
    value blob not null,

    primary key (inode_id, name),
    foreign key (inode_id) references inode(id) on delete cascade
);

-- Indexes for performance
create index idx_link_child on link(child_id);
create index idx_block_range on block(inode_id, first_byte, last_byte);
create index idx_inode_mode on inode(mode);