#!/usr/bin/fish

for i in (seq 1 3)
    cd $HOME/Development/silvershore

    claude --dangerously-skip-permissions -- \
        "@$HOME/Documents/todo/shore-gui/shore-gui-$i.md"
end
