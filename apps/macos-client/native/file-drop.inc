@protocol LRDFileDropDelegate <NSObject>
- (BOOL)acceptFileURLs:(NSArray<NSURL *> *)urls;
@end
@interface LRDFileDropView:NSView
@property(weak) id<LRDFileDropDelegate> fileDelegate;
@end
@implementation LRDFileDropView
- (NSDragOperation)draggingEntered:(id<NSDraggingInfo>)sender {
    NSArray *urls=[sender.draggingPasteboard readObjectsForClasses:@[[NSURL class]] options:@{NSPasteboardURLReadingFileURLsOnlyKey:@YES}];
    return urls.count>0 && urls.count<=4 ? NSDragOperationCopy : NSDragOperationNone;
}
- (BOOL)performDragOperation:(id<NSDraggingInfo>)sender {
    NSArray *urls=[sender.draggingPasteboard readObjectsForClasses:@[[NSURL class]] options:@{NSPasteboardURLReadingFileURLsOnlyKey:@YES}];
    return urls.count>0 && urls.count<=4 && [self.fileDelegate acceptFileURLs:urls];
}
@end
