use std::{
    cell::Cell,
    fmt,
    hash::{Hash, Hasher},
    iter, mem, ptr,
};

use countme::Count;

use crate::{
    green::{GreenElementRef, GreenNodeData, SyntaxKind},
    Children, Direction, GreenNode, GreenToken, NodeOrToken, SyntaxText, TextRange, TextSize,
    TokenAtOffset, WalkEvent,
};

pub struct SyntaxNode {
    ptr: ptr::NonNull<NodeData>,
}

impl Clone for SyntaxNode {
    #[inline]
    fn clone(&self) -> Self {
        let rc = match self.data().rc.get().checked_add(1) {
            Some(it) => it,
            None => std::process::abort(),
        };
        self.data().rc.set(rc);
        SyntaxNode { ptr: self.ptr }
    }
}

impl Drop for SyntaxNode {
    #[inline]
    fn drop(&mut self) {
        let rc = self.data().rc.get() - 1;
        self.data().rc.set(rc);
        if rc == 0 {
            unsafe { free(Box::from_raw(self.ptr.as_ptr())) }
        }
    }
}

#[inline(never)]
fn free(mut data: Box<NodeData>) {
    loop {
        debug_assert_eq!(data.rc.get(), 0);
        match data.parent.take() {
            Some(parent) => {
                let parent = mem::ManuallyDrop::new(parent);
                let rc = parent.data().rc.get() - 1;
                parent.data().rc.set(rc);
                if rc == 0 {
                    data = unsafe { Box::from_raw(parent.ptr.as_ptr()) }
                } else {
                    break;
                }
            }
            None => unsafe {
                GreenNode::from_raw(data.green);
                break;
            },
        }
    }
}

// Identity semantics for hash & eq
impl PartialEq for SyntaxNode {
    #[inline]
    fn eq(&self, other: &SyntaxNode) -> bool {
        self.key() == other.key()
    }
}

impl Eq for SyntaxNode {}

impl Hash for SyntaxNode {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl fmt::Debug for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyntaxNode")
            .field("kind", &self.kind())
            .field("text_range", &self.text_range())
            .finish()
    }
}

impl fmt::Display for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.preorder_with_tokens()
            .filter_map(|event| match event {
                WalkEvent::Enter(NodeOrToken::Token(token)) => Some(token),
                _ => None,
            })
            .try_for_each(|it| fmt::Display::fmt(&it, f))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SyntaxToken {
    parent: SyntaxNode,
    index: u32,
    offset: TextSize,
}

impl fmt::Display for SyntaxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.text(), f)
    }
}

pub type SyntaxElement = NodeOrToken<SyntaxNode, SyntaxToken>;

impl From<SyntaxNode> for SyntaxElement {
    #[inline]
    fn from(node: SyntaxNode) -> SyntaxElement {
        NodeOrToken::Node(node)
    }
}

impl From<SyntaxToken> for SyntaxElement {
    #[inline]
    fn from(token: SyntaxToken) -> SyntaxElement {
        NodeOrToken::Token(token)
    }
}

impl fmt::Display for SyntaxElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeOrToken::Node(it) => fmt::Display::fmt(it, f),
            NodeOrToken::Token(it) => fmt::Display::fmt(it, f),
        }
    }
}

struct NodeData {
    rc: Cell<u32>,
    parent: Option<SyntaxNode>,
    index: u32,
    offset: TextSize,
    green: ptr::NonNull<GreenNodeData>,
    _c: Count<SyntaxNode>,
}

impl SyntaxNode {
    fn new(data: NodeData) -> SyntaxNode {
        SyntaxNode { ptr: unsafe { ptr::NonNull::new_unchecked(Box::into_raw(Box::new(data))) } }
    }

    pub fn new_root(green: GreenNode) -> SyntaxNode {
        let data = NodeData {
            rc: Cell::new(1),
            parent: None,
            index: 0,
            offset: 0.into(),
            green: GreenNode::into_raw(green),
            _c: Count::new(),
        };
        SyntaxNode::new(data)
    }

    // Technically, unsafe, but private so that's OK.
    // Safety: `green` must be a descendent of `parent.green()`
    fn new_child(
        green: &GreenNode,
        parent: SyntaxNode,
        index: u32,
        offset: TextSize,
    ) -> SyntaxNode {
        let data = NodeData {
            rc: Cell::new(1),
            parent: Some(parent),
            index,
            offset,
            green: {
                let green: &GreenNodeData = &*green;
                ptr::NonNull::from(green)
            },
            _c: Count::new(),
        };
        SyntaxNode::new(data)
    }

    fn key(&self) -> (ptr::NonNull<GreenNodeData>, TextSize) {
        (self.data().green, self.data().offset)
    }

    #[inline]
    fn data(&self) -> &NodeData {
        unsafe { self.ptr.as_ref() }
    }

    pub fn replace_with(&self, replacement: GreenNode) -> GreenNode {
        assert_eq!(self.kind(), replacement.kind());
        match &self.data().parent {
            None => replacement,
            Some(parent) => {
                let new_parent =
                    parent.green().replace_child(self.data().index as usize, replacement.into());
                parent.replace_with(new_parent)
            }
        }
    }

    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.green().kind()
    }

    #[inline]
    pub fn text_range(&self) -> TextRange {
        let offset = self.data().offset;
        let len = self.green().text_len();
        TextRange::at(offset, len)
    }

    #[inline]
    pub fn text(&self) -> SyntaxText {
        SyntaxText::new(self.clone())
    }

    #[inline]
    pub fn green(&self) -> &GreenNodeData {
        unsafe { self.data().green.as_ref() }
    }

    #[inline]
    pub fn parent(&self) -> Option<SyntaxNode> {
        self.data().parent.clone()
    }

    #[inline]
    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> {
        iter::successors(Some(self.clone()), SyntaxNode::parent)
    }

    #[inline]
    pub fn children(&self) -> SyntaxNodeChildren {
        SyntaxNodeChildren::new(self.clone())
    }

    #[inline]
    pub fn children_with_tokens(&self) -> SyntaxElementChildren {
        SyntaxElementChildren::new(self.clone())
    }

    pub fn first_child(&self) -> Option<SyntaxNode> {
        let (node, (index, offset)) =
            filter_nodes(self.green().children_from(0, self.text_range().start())).next()?;

        Some(SyntaxNode::new_child(node, self.clone(), index as u32, offset))
    }

    pub fn first_child_or_token(&self) -> Option<SyntaxElement> {
        let (element, (index, offset)) =
            self.green().children_from(0, self.text_range().start()).next()?;
        Some(SyntaxElement::new(element, self.clone(), index as u32, offset))
    }

    pub fn last_child(&self) -> Option<SyntaxNode> {
        let (node, (index, offset)) = filter_nodes(
            self.green().children_to(self.green().children().len(), self.text_range().end()),
        )
        .next()?;

        Some(SyntaxNode::new_child(node, self.clone(), index as u32, offset))
    }

    pub fn last_child_or_token(&self) -> Option<SyntaxElement> {
        let (element, (index, offset)) = self
            .green()
            .children_to(self.green().children().len(), self.text_range().end())
            .next()?;
        Some(SyntaxElement::new(element, self.clone(), index as u32, offset))
    }

    pub fn next_sibling(&self) -> Option<SyntaxNode> {
        let parent = self.data().parent.as_ref()?;

        let (node, (index, offset)) = filter_nodes(
            parent.green().children_from((self.data().index + 1) as usize, self.text_range().end()),
        )
        .next()?;

        Some(SyntaxNode::new_child(node, parent.clone(), index as u32, offset))
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        let parent = self.data().parent.as_ref()?;

        let (element, (index, offset)) = parent
            .green()
            .children_from((self.data().index + 1) as usize, self.text_range().end())
            .next()?;

        Some(SyntaxElement::new(element, parent.clone(), index as u32, offset))
    }

    pub fn prev_sibling(&self) -> Option<SyntaxNode> {
        let parent = self.data().parent.as_ref()?;

        let (node, (index, offset)) = filter_nodes(
            parent.green().children_to(self.data().index as usize, self.text_range().start()),
        )
        .next()?;

        Some(SyntaxNode::new_child(node, parent.clone(), index as u32, offset))
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        let parent = self.data().parent.as_ref()?;

        let (element, (index, offset)) = parent
            .green()
            .children_to(self.data().index as usize, self.text_range().start())
            .next()?;

        Some(SyntaxElement::new(element, parent.clone(), index as u32, offset))
    }

    pub fn first_token(&self) -> Option<SyntaxToken> {
        self.first_child_or_token()?.first_token()
    }

    pub fn last_token(&self) -> Option<SyntaxToken> {
        self.last_child_or_token()?.last_token()
    }

    #[inline]
    pub fn siblings(&self, direction: Direction) -> impl Iterator<Item = SyntaxNode> {
        iter::successors(Some(self.clone()), move |node| match direction {
            Direction::Next => node.next_sibling(),
            Direction::Prev => node.prev_sibling(),
        })
    }

    #[inline]
    pub fn siblings_with_tokens(
        &self,
        direction: Direction,
    ) -> impl Iterator<Item = SyntaxElement> {
        let me: SyntaxElement = self.clone().into();
        iter::successors(Some(me), move |el| match direction {
            Direction::Next => el.next_sibling_or_token(),
            Direction::Prev => el.prev_sibling_or_token(),
        })
    }

    #[inline]
    pub fn descendants(&self) -> impl Iterator<Item = SyntaxNode> {
        self.preorder().filter_map(|event| match event {
            WalkEvent::Enter(node) => Some(node),
            WalkEvent::Leave(_) => None,
        })
    }

    #[inline]
    pub fn descendants_with_tokens(&self) -> impl Iterator<Item = SyntaxElement> {
        self.preorder_with_tokens().filter_map(|event| match event {
            WalkEvent::Enter(it) => Some(it),
            WalkEvent::Leave(_) => None,
        })
    }

    #[inline]
    pub fn preorder(&self) -> Preorder {
        Preorder::new(self.clone())
    }

    #[inline]
    pub fn preorder_with_tokens<'a>(&'a self) -> impl Iterator<Item = WalkEvent<SyntaxElement>> {
        let start: SyntaxElement = self.clone().into();
        iter::successors(Some(WalkEvent::Enter(start.clone())), move |pos| {
            let next = match pos {
                WalkEvent::Enter(el) => match el {
                    NodeOrToken::Node(node) => match node.first_child_or_token() {
                        Some(child) => WalkEvent::Enter(child),
                        None => WalkEvent::Leave(node.clone().into()),
                    },
                    NodeOrToken::Token(token) => WalkEvent::Leave(token.clone().into()),
                },
                WalkEvent::Leave(el) => {
                    if el == &start {
                        return None;
                    }
                    match el.next_sibling_or_token() {
                        Some(sibling) => WalkEvent::Enter(sibling),
                        None => WalkEvent::Leave(el.parent().unwrap().into()),
                    }
                }
            };
            Some(next)
        })
    }

    pub fn token_at_offset(&self, offset: TextSize) -> TokenAtOffset<SyntaxToken> {
        // TODO: this could be faster if we first drill-down to node, and only
        // then switch to token search. We should also replace explicit
        // recursion with a loop.
        let range = self.text_range();
        assert!(
            range.start() <= offset && offset <= range.end(),
            "Bad offset: range {:?} offset {:?}",
            range,
            offset
        );
        if range.is_empty() {
            return TokenAtOffset::None;
        }

        let mut children = self.children_with_tokens().filter(|child| {
            let child_range = child.text_range();
            !child_range.is_empty()
                && (child_range.start() <= offset && offset <= child_range.end())
        });

        let left = children.next().unwrap();
        let right = children.next();
        assert!(children.next().is_none());

        if let Some(right) = right {
            match (left.token_at_offset(offset), right.token_at_offset(offset)) {
                (TokenAtOffset::Single(left), TokenAtOffset::Single(right)) => {
                    TokenAtOffset::Between(left, right)
                }
                _ => unreachable!(),
            }
        } else {
            left.token_at_offset(offset)
        }
    }

    pub fn covering_element(&self, range: TextRange) -> SyntaxElement {
        let mut res: SyntaxElement = self.clone().into();
        loop {
            assert!(
                res.text_range().contains_range(range),
                "Bad range: node range {:?}, range {:?}",
                res.text_range(),
                range,
            );
            res = match &res {
                NodeOrToken::Token(_) => return res,
                NodeOrToken::Node(node) => match node.child_or_token_at_range(range) {
                    Some(it) => it,
                    None => return res,
                },
            };
        }
    }

    pub fn child_or_token_at_range(&self, range: TextRange) -> Option<SyntaxElement> {
        let start_offset = self.text_range().start();
        let (index, offset, child) = self.green().child_at_range(range - start_offset)?;
        let index = index as u32;
        let offset = offset + start_offset;
        let res: SyntaxElement = match child {
            NodeOrToken::Node(node) => {
                SyntaxNode::new_child(node.into(), self.clone(), index, offset).into()
            }
            NodeOrToken::Token(_token) => SyntaxToken::new(self.clone(), index, offset).into(),
        };
        Some(res)
    }
}

impl SyntaxToken {
    fn new(parent: SyntaxNode, index: u32, offset: TextSize) -> SyntaxToken {
        SyntaxToken { parent, index, offset }
    }

    pub fn replace_with(&self, replacement: GreenToken) -> GreenNode {
        assert_eq!(self.kind(), replacement.kind());
        let mut replacement = Some(replacement);
        let parent = self.parent();
        let me = self.index;

        let children = parent.green().children().enumerate().map(|(i, child)| {
            if i as u32 == me {
                replacement.take().unwrap().into()
            } else {
                child.cloned()
            }
        });
        let new_parent = GreenNode::new(parent.kind(), children);
        parent.replace_with(new_parent)
    }

    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.green().kind()
    }

    #[inline]
    pub fn text_range(&self) -> TextRange {
        TextRange::at(self.offset, self.green().text_len())
    }

    #[inline]
    pub fn text(&self) -> &str {
        self.green().text()
    }

    #[inline]
    pub fn green(&self) -> &GreenToken {
        self.parent.green().children().nth(self.index as usize).unwrap().as_token().unwrap()
    }

    #[inline]
    pub fn parent(&self) -> SyntaxNode {
        self.parent.clone()
    }

    #[inline]
    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> {
        self.parent().ancestors()
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        let (element, (index, offset)) = self
            .parent
            .green()
            .children_from((self.index + 1) as usize, self.text_range().end())
            .next()?;

        Some(SyntaxElement::new(element, self.parent(), index as u32, offset))
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        let parent = self.parent();
        let (element, (index, offset)) = self
            .parent
            .green()
            .children_to(self.index as usize, self.text_range().start())
            .next()?;

        Some(SyntaxElement::new(element, parent, index as u32, offset))
    }

    pub fn siblings_with_tokens(
        &self,
        direction: Direction,
    ) -> impl Iterator<Item = SyntaxElement> {
        let me: SyntaxElement = self.clone().into();
        iter::successors(Some(me), move |el| match direction {
            Direction::Next => el.next_sibling_or_token(),
            Direction::Prev => el.prev_sibling_or_token(),
        })
    }

    pub fn next_token(&self) -> Option<SyntaxToken> {
        match self.next_sibling_or_token() {
            Some(element) => element.first_token(),
            None => self
                .parent()
                .ancestors()
                .find_map(|it| it.next_sibling_or_token())
                .and_then(|element| element.first_token()),
        }
    }

    pub fn prev_token(&self) -> Option<SyntaxToken> {
        match self.prev_sibling_or_token() {
            Some(element) => element.last_token(),
            None => self
                .parent()
                .ancestors()
                .find_map(|it| it.prev_sibling_or_token())
                .and_then(|element| element.last_token()),
        }
    }
}

impl SyntaxElement {
    fn new(
        element: GreenElementRef<'_>,
        parent: SyntaxNode,
        index: u32,
        offset: TextSize,
    ) -> SyntaxElement {
        match element {
            NodeOrToken::Node(node) => {
                SyntaxNode::new_child(node, parent, index as u32, offset).into()
            }
            NodeOrToken::Token(_) => SyntaxToken::new(parent, index as u32, offset).into(),
        }
    }

    #[inline]
    pub fn text_range(&self) -> TextRange {
        match self {
            NodeOrToken::Node(it) => it.text_range(),
            NodeOrToken::Token(it) => it.text_range(),
        }
    }

    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        match self {
            NodeOrToken::Node(it) => it.kind(),
            NodeOrToken::Token(it) => it.kind(),
        }
    }

    #[inline]
    pub fn parent(&self) -> Option<SyntaxNode> {
        match self {
            NodeOrToken::Node(it) => it.parent(),
            NodeOrToken::Token(it) => Some(it.parent()),
        }
    }

    #[inline]
    pub fn ancestors(&self) -> impl Iterator<Item = SyntaxNode> {
        match self {
            NodeOrToken::Node(it) => it.ancestors(),
            NodeOrToken::Token(it) => it.parent().ancestors(),
        }
    }

    pub fn first_token(&self) -> Option<SyntaxToken> {
        match self {
            NodeOrToken::Node(it) => it.first_token(),
            NodeOrToken::Token(it) => Some(it.clone()),
        }
    }

    pub fn last_token(&self) -> Option<SyntaxToken> {
        match self {
            NodeOrToken::Node(it) => it.last_token(),
            NodeOrToken::Token(it) => Some(it.clone()),
        }
    }

    pub fn next_sibling_or_token(&self) -> Option<SyntaxElement> {
        match self {
            NodeOrToken::Node(it) => it.next_sibling_or_token(),
            NodeOrToken::Token(it) => it.next_sibling_or_token(),
        }
    }

    pub fn prev_sibling_or_token(&self) -> Option<SyntaxElement> {
        match self {
            NodeOrToken::Node(it) => it.prev_sibling_or_token(),
            NodeOrToken::Token(it) => it.prev_sibling_or_token(),
        }
    }

    fn token_at_offset(&self, offset: TextSize) -> TokenAtOffset<SyntaxToken> {
        assert!(self.text_range().start() <= offset && offset <= self.text_range().end());
        match self {
            NodeOrToken::Token(token) => TokenAtOffset::Single(token.clone()),
            NodeOrToken::Node(node) => node.token_at_offset(offset),
        }
    }
}

#[derive(Clone, Debug)]
struct Iter {
    parent: SyntaxNode,
    green: Children<'static>,
    offset: TextSize,
    index: u32,
}

impl Iter {
    fn new(parent: SyntaxNode) -> Iter {
        let offset = parent.text_range().start();
        let green: Children<'_> = parent.green().children();
        // Dirty lifetime laundering: the memory for the children is
        // indirectly owned by parent.
        let green: Children<'static> =
            unsafe { mem::transmute::<Children<'_>, Children<'static>>(green) };
        Iter { parent, green, offset, index: 0 }
    }

    fn next(&mut self) -> Option<(GreenElementRef, u32, TextSize)> {
        self.green.next().map(|element| {
            let offset = self.offset;
            let index = self.index;
            self.offset += element.text_len();
            self.index += 1;
            (element, index, offset)
        })
    }
}

#[derive(Clone, Debug)]
pub struct SyntaxNodeChildren(Iter);

impl SyntaxNodeChildren {
    fn new(parent: SyntaxNode) -> SyntaxNodeChildren {
        SyntaxNodeChildren(Iter::new(parent))
    }
}

impl Iterator for SyntaxNodeChildren {
    type Item = SyntaxNode;
    fn next(&mut self) -> Option<Self::Item> {
        let parent = self.0.parent.clone();
        while let Some((element, index, offset)) = self.0.next() {
            if let Some(node) = element.as_node() {
                return Some(SyntaxNode::new_child(node, parent, index, offset));
            }
        }
        None
    }
}

#[derive(Clone, Debug)]
pub struct SyntaxElementChildren(Iter);

impl SyntaxElementChildren {
    fn new(parent: SyntaxNode) -> SyntaxElementChildren {
        SyntaxElementChildren(Iter::new(parent))
    }
}

impl Iterator for SyntaxElementChildren {
    type Item = SyntaxElement;
    fn next(&mut self) -> Option<Self::Item> {
        let parent = self.0.parent.clone();
        self.0.next().map(|(green, index, offset)| SyntaxElement::new(green, parent, index, offset))
    }
}

impl GreenNodeData {
    fn children_from(
        &self,
        start_index: usize,
        mut offset: TextSize,
    ) -> impl Iterator<Item = (GreenElementRef, (usize, TextSize))> {
        self.children().skip(start_index).enumerate().map(move |(index, element)| {
            let element_offset = offset;
            offset += element.text_len();
            (element, (start_index + index, element_offset))
        })
    }

    fn children_to(
        &self,
        end_index: usize,
        mut offset: TextSize,
    ) -> impl Iterator<Item = (GreenElementRef, (usize, TextSize))> {
        self.children().take(end_index).rev().enumerate().map(move |(index, element)| {
            offset -= element.text_len();
            (element, (end_index - index - 1, offset))
        })
    }
}

fn filter_nodes<'a, I: Iterator<Item = (GreenElementRef<'a>, T)>, T>(
    iter: I,
) -> impl Iterator<Item = (&'a GreenNode, T)> {
    iter.filter_map(|(element, data)| match element {
        NodeOrToken::Node(it) => Some((it, data)),
        NodeOrToken::Token(_) => None,
    })
}

pub struct Preorder {
    root: SyntaxNode,
    next: Option<WalkEvent<SyntaxNode>>,
    skip_subtree: bool,
}

impl Preorder {
    fn new(root: SyntaxNode) -> Preorder {
        let next = Some(WalkEvent::Enter(root.clone()));
        Preorder { root, next, skip_subtree: false }
    }

    pub fn skip_subtree(&mut self) {
        self.skip_subtree = true;
    }
    #[cold]
    fn do_skip(&mut self) {
        self.next = self.next.take().map(|next| match next {
            WalkEvent::Enter(first_child) => WalkEvent::Leave(first_child.parent().unwrap()),
            WalkEvent::Leave(parent) => WalkEvent::Leave(parent),
        })
    }
}

impl Iterator for Preorder {
    type Item = WalkEvent<SyntaxNode>;

    fn next(&mut self) -> Option<WalkEvent<SyntaxNode>> {
        if self.skip_subtree {
            self.do_skip();
            self.skip_subtree = false;
        }
        let next = self.next.take();
        self.next = next.as_ref().and_then(|next| {
            Some(match next {
                WalkEvent::Enter(node) => match node.first_child() {
                    Some(child) => WalkEvent::Enter(child),
                    None => WalkEvent::Leave(node.clone()),
                },
                WalkEvent::Leave(node) => {
                    if node == &self.root {
                        return None;
                    }
                    match node.next_sibling() {
                        Some(sibling) => WalkEvent::Enter(sibling),
                        None => WalkEvent::Leave(node.parent().unwrap()),
                    }
                }
            })
        });
        next
    }
}
