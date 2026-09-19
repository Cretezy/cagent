ALTER TABLE composer_history ADD COLUMN images_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE composer_history ADD COLUMN image_chips_json TEXT NOT NULL DEFAULT '[]';
