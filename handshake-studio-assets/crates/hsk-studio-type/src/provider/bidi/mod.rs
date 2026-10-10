mod char_data;
mod data_source;
mod explicit;
mod implicit;
mod prepare;
pub(crate) mod level;
mod format_chars;
pub(crate) use char_data::{BidiClass,UNICODE_VERSION};
pub(crate) use char_data::bidi_class as class;
use char_data::HardcodedBidiData;
use data_source::BidiDataSource;
use level::Level;
use prepare::{LevelRunVec,IsolatingRunSequenceVec};
use crate::{Error,provider::{Context,ProviderResult,storage::ChargedVec}};
use std::ops::Range;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum StyleOverride { Default,Ltr,Rtl }
#[derive(Clone,Copy)]
pub(crate) struct Paragraph { pub start:usize,pub end:usize,pub level:Level }
pub(crate) struct BidiResult<'a> {
    pub classes:ChargedVec<'a,BidiClass>,
    pub levels:ChargedVec<'a,Level>,
    pub paragraphs:ChargedVec<'a,Paragraph>,
}
pub(crate) trait TextSource<'text> {
    type CharIter:Iterator<Item=char>;
    type CharIndexIter:Iterator<Item=(usize,char)>;
    type IndexLenIter:Iterator<Item=(usize,usize)>;
    fn len(&self)->usize;
    fn char_at(&self,index:usize)->Option<(char,usize)>;
    fn subrange(&self,range:Range<usize>)->&Self;
    fn chars(&'text self)->Self::CharIter;
    fn char_indices(&'text self)->Self::CharIndexIter;
    fn indices_lengths(&'text self)->Self::IndexLenIter;
    fn char_len(ch:char)->usize;
}
pub(crate) struct IndexLengths<'a>(std::str::CharIndices<'a>);
impl Iterator for IndexLengths<'_> {
    type Item=(usize,usize);
    fn next(&mut self)->Option<Self::Item> { self.0.next().map(|(i,c)|(i,c.len_utf8())) }
}
impl<'a> TextSource<'a> for str {
    type CharIter=std::str::Chars<'a>;
    type CharIndexIter=std::str::CharIndices<'a>;
    type IndexLenIter=IndexLengths<'a>;
    fn len(&self)->usize { str::len(self) }
    fn char_at(&self,i:usize)->Option<(char,usize)> { self.get(i..)?.chars().next().map(|c|(c,c.len_utf8())) }
    fn subrange(&self,r:Range<usize>)->&Self { &self[r] }
    fn chars(&'a self)->Self::CharIter { str::chars(self) }
    fn char_indices(&'a self)->Self::CharIndexIter { str::char_indices(self) }
    fn indices_lengths(&'a self)->Self::IndexLenIter { IndexLengths(str::char_indices(self)) }
    fn char_len(ch:char)->usize { ch.len_utf8() }
}
/// Original UTF8 bytes are the sole coordinate system; style overrides enter only X6.
pub(crate) fn resolve<'a>(text:&str,styles:&[StyleOverride],paragraph_levels:&[Level],ctx:&Context<'a>)->ProviderResult<BidiResult<'a>> {
    use BidiClass::*;
    if text.is_empty() || styles.len()!=text.len() || paragraph_levels.is_empty() { return Err(Error::InvalidInput); }
    let data=HardcodedBidiData;
    let mut classes=ChargedVec::with_capacity(text.len(),ctx)?;
    let mut paragraphs=ChargedVec::new();
    let mut isolates=ChargedVec::new();
    let mut start=0;
    for (i,c) in text.char_indices() {
        ctx.step()?;
        let class=data.bidi_class(c,ctx)?;
        for byte in i..i+c.len_utf8() {
            ctx.step()?;
            if styles[byte]!=styles[i] { return Err(Error::InvalidBidi); }
            classes.push(class,ctx)?;
        }
        match class {
            B=> { paragraphs.push(Paragraph {start,end:i+c.len_utf8(),level:*paragraph_levels.get(paragraphs.len()).ok_or(Error::InvalidBidi)?},ctx)?; start=i+c.len_utf8();isolates.clear(); }
            L|R|AL=> {
                if let Some(&opening)=isolates.last() {
                    if classes[opening]==FSI {
                        for j in opening..opening+format_chars::FSI.len_utf8() { ctx.step()?;classes[j]=if class==L { LRI }else{ RLI }; }
                    }
                }
            }
            LRI|RLI|FSI=>isolates.push(i,ctx)?,
            PDI=>{ isolates.pop(); },
            _=>{},
        }
    }
    if start<text.len() { paragraphs.push(Paragraph {start,end:text.len(),level:*paragraph_levels.get(paragraphs.len()).ok_or(Error::InvalidBidi)?},ctx)?; }
    if paragraphs.len()!=paragraph_levels.len() { return Err(Error::InvalidBidi); }
    let mut processing=ChargedVec::new();processing.extend_copy(classes.as_slice(),ctx)?;
    let mut levels=ChargedVec::new();levels.resize_copy(text.len(),Level::ltr(),ctx)?;
    for paragraph in paragraphs.iter() {
        ctx.step()?;
        let range=paragraph.start..paragraph.end;
        let original=&classes[range.clone()];
        let pclasses=&mut processing[range.clone()];
        let plevels=&mut levels[range.clone()];
        for level in plevels.iter_mut() { ctx.step()?;*level=paragraph.level; }
        let ptext=&text[range.clone()];
        let mut runs=LevelRunVec::new();
        explicit::compute(ptext,paragraph.level,original,plevels,pclasses,&mut runs,&styles[range],ctx)?;
        let mut sequences=IsolatingRunSequenceVec::new();
        prepare::isolating_run_sequences(paragraph.level,original,plevels,runs,true,&mut sequences,ctx)?;
        for sequence in sequences.iter() {
            ctx.step()?;
            implicit::resolve_weak(ptext,sequence,pclasses,ctx)?;
            implicit::resolve_neutral(ptext,&data,sequence,plevels,original,pclasses,ctx)?;
        }
        implicit::resolve_levels(pclasses,plevels,ctx)?;
        for i in 0..plevels.len() { ctx.step()?; if prepare::removed_by_x9(original[i]) { plevels[i]=if i>0 {plevels[i-1]}else{paragraph.level}; } }
    }
    Ok(BidiResult {classes,levels,paragraphs})
}
/// L1 then L2 on one caller-selected line; run ranges retain original byte offsets.
pub(crate) fn visual_runs<'a>(result:&mut BidiResult<'a>,text:&str,paragraph:Paragraph,line:Range<usize>,ctx:&Context<'a>)->ProviderResult<LevelRunVec<'a>> {
    use BidiClass::*;
    if line.start<paragraph.start || line.end>paragraph.end || line.is_empty()
        || !text.is_char_boundary(line.start) || !text.is_char_boundary(line.end)
        || result.levels.len()!=text.len() || result.classes.len()!=text.len() { return Err(Error::InvalidBidi); }
    let mut reset_from=Some(line.start);
    let mut reset_to=None;
    let mut previous=paragraph.level;
    for (offset,c) in text[line.clone()].char_indices() {
        ctx.step()?;
        let i=line.start+offset;
        match result.classes[i] {
            B|S=> {reset_to=Some(i+c.len_utf8()); if reset_from.is_none(){reset_from=Some(i);} },
            WS|FSI|LRI|RLI|PDI=> { if reset_from.is_none(){reset_from=Some(i);} },
            RLE|LRE|RLO|LRO|PDF|BN=> {
                if reset_from.is_none(){reset_from=Some(i);}
                for j in i..i+c.len_utf8() {ctx.step()?;result.levels[j]=previous;}
            }
            _=>{reset_from=None;},
        }
        if let (Some(from),Some(to))=(reset_from,reset_to) {
            for j in from..to {ctx.step()?;result.levels[j]=paragraph.level;}
            reset_from=None;reset_to=None;
        }
        previous=result.levels[i];
    }
    if let Some(from)=reset_from {
        for j in from..line.end {ctx.step()?;result.levels[j]=paragraph.level;}
    }
    let mut runs=LevelRunVec::new();
    let mut start=line.start;
    let mut run_level=result.levels[start];
    let mut minimum=run_level;
    let mut maximum=run_level;
    for i in start+1..line.end {
        ctx.step()?;
        let level=result.levels[i];
        if level!=run_level {
            runs.push(start..i,ctx)?;start=i;run_level=level;
            minimum=std::cmp::min(level,minimum);maximum=std::cmp::max(level,maximum);
        }
    }
    runs.push(start..line.end,ctx)?;
    let minimum=minimum.new_lowest_ge_rtl().map_err(|_|Error::InvalidBidi)?;
    while maximum>=minimum {
        ctx.step()?;
        let mut left=0;
        while left<runs.len() {
            ctx.step()?;
            if result.levels[runs[left].start]<maximum {left+=1;continue;}
            let mut right=left+1;
            while right<runs.len() {
                ctx.step()?;
                if result.levels[runs[right].start]<maximum {break;}
                right+=1;
            }
            let mut a=left;let mut b=right;
            while a<b {
                ctx.step()?;b-=1;
                if a<b {runs.swap(a,b);a+=1;}
            }
            left=right;
        }
        maximum.lower(1).map_err(|_|Error::InvalidBidi)?;
    }
    Ok(runs)
}
