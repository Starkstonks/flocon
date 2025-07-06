use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create inode table
        manager
            .create_table(
                Table::create()
                    .table(Inode::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Inode::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Inode::Mode).integer().not_null())
                    .col(ColumnDef::new(Inode::Uid).integer().not_null())
                    .col(ColumnDef::new(Inode::Gid).integer().not_null())
                    .col(ColumnDef::new(Inode::Size).integer().not_null().default(0))
                    .col(ColumnDef::new(Inode::Atime).date_time().not_null())
                    .col(ColumnDef::new(Inode::Mtime).date_time().not_null())
                    .col(ColumnDef::new(Inode::Ctime).date_time().not_null())
                    .col(ColumnDef::new(Inode::Btime).date_time().not_null())
                    .to_owned(),
            )
            .await?;

        // Create link table
        manager
            .create_table(
                Table::create()
                    .table(Link::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Link::ParentId).integer().not_null())
                    .col(ColumnDef::new(Link::ChildId).integer().not_null())
                    .col(ColumnDef::new(Link::Name).string().not_null())
                    .primary_key(Index::create().col(Link::ParentId).col(Link::Name))
                    .foreign_key(
                        ForeignKey::create()
                            .from(Link::Table, Link::ParentId)
                            .to(Inode::Table, Inode::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Link::Table, Link::ChildId)
                            .to(Inode::Table, Inode::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create block table
        manager
            .create_table(
                Table::create()
                    .table(Block::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Block::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Block::InodeId).integer().not_null())
                    .col(ColumnDef::new(Block::FirstByte).integer().not_null())
                    .col(ColumnDef::new(Block::LastByte).integer().not_null())
                    .col(ColumnDef::new(Block::Data).blob())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Block::Table, Block::InodeId)
                            .to(Inode::Table, Inode::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(Block::FirstByte).lte(Expr::col(Block::LastByte)))
                    .to_owned(),
            )
            .await?;

        // Create xattr table
        manager
            .create_table(
                Table::create()
                    .table(Xattr::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Xattr::InodeId).integer().not_null())
                    .col(ColumnDef::new(Xattr::Name).string().not_null())
                    .col(ColumnDef::new(Xattr::Value).blob().not_null())
                    .primary_key(Index::create().col(Xattr::InodeId).col(Xattr::Name))
                    .foreign_key(
                        ForeignKey::create()
                            .from(Xattr::Table, Xattr::InodeId)
                            .to(Inode::Table, Inode::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes
        manager
            .create_index(
                Index::create()
                    .name("idx_link_child")
                    .table(Link::Table)
                    .col(Link::ChildId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_block_range")
                    .table(Block::Table)
                    .col(Block::InodeId)
                    .col(Block::FirstByte)
                    .col(Block::LastByte)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_inode_mode")
                    .table(Inode::Table)
                    .col(Inode::Mode)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Xattr::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Block::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Link::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Inode::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(Iden)]
enum Inode {
    Table,
    Id,
    Mode,
    Uid,
    Gid,
    Size,
    Atime,
    Mtime,
    Ctime,
    Btime,
}

#[derive(Iden)]
enum Link {
    Table,
    ParentId,
    ChildId,
    Name,
}

#[derive(Iden)]
enum Block {
    Table,
    Id,
    InodeId,
    FirstByte,
    LastByte,
    Data,
}

#[derive(Iden)]
enum Xattr {
    Table,
    InodeId,
    Name,
    Value,
}
