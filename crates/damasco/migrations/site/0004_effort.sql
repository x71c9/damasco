-- The effort chosen for a conversation, as the assistant's CLI names it
-- (`high`, `xhigh`...); the model's default when null.
ALTER TABLE conversation ADD COLUMN effort TEXT;
