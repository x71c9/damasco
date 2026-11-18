-- The version of the site a chat line refers to: for a message of a
-- person, the site before the message, to go back to; for the line that
-- tells a going back, the site before it, to undo it.
ALTER TABLE chat_item ADD COLUMN version TEXT;
