CREATE UNIQUE INDEX IF NOT EXISTS idx_client_turns_app_id
    ON turns(thread_id, app_server_turn_id)
    WHERE app_server_turn_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_client_items_app_id
    ON items(turn_id, app_server_item_id)
    WHERE app_server_item_id IS NOT NULL;
