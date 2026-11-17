-- The model chosen for the conversation, as the assistant's CLI names it;
-- none for the CLI's default.
ALTER TABLE conversation ADD COLUMN model TEXT;
