- Data grid editing + context menu:

> I guess ideally it should just bind the right click to the whole grid rather than the cell or row if possible. Also seems like the *first* right click on a grid doesn't trigger it, only subsequent right clicks do.
> Good enough I guess, but if it can be cleanly improved, that'd be good.
> Deleted rows, edited cells, and inserted rows that are not yet committed should all show with colored backgrounds (red for deleted, green for edited, blue for new). All changes should be tracked until committed, and committing should (for now) just be a button to confirm/apply the visually-represented changes instead of showing all of the queries that will be executed.

- Alternating background row colors for results - toggle in preferences
- Double clicking connection from welcome view should open it immediately instead of just showing the connection settings
- Support middle mouse drag in SQL editor for multi-line select, like Zed
- macOS menu bar basic support (nothing significant since we don't have a menu bar on other platforms yet)
