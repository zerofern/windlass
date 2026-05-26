# Windlass Pipeline PoC

Manual exploration of the recommendation pipeline before committing to the full architecture.
No data storage. Terminal for API calls, Gemini webapp for LLM prompts.

## Goals

- Validate what Audnexus and Hardcover actually return for real books
- Understand tag normalization difficulty
- Test Stage 0.5 dot-product scoring logic with real tags
- Test Stage 1 TUI-structured prompt quality
- Test Decide two-pass prompt (TKR ranking + blurb)
- Test Learn IR-structured prompt profile update
- Document everything that breaks or surprises us

## Stages

1. [API Reachability](#1-api-reachability)
2. [Tag Normalization](#2-tag-normalization)
3. [Stage 0.5 — Structured Scoring](#3-stage-05--structured-scoring)
4. [Stage 1 — LLM Enrichment (TUI)](#4-stage-1--llm-enrichment-tui)
5. [Decide — Two-Pass](#5-decide--two-pass)
6. [Learn — IR Profile Update](#6-learn--ir-profile-update)

---

## Seed Books

Drawn from `user-profile-example.md`. Hall of Fame books are used for Learn call tests; Mixed Bag for negative signal tests.

| Title | Author | ASIN | ISBN | Ledger | Notes |
|-------|--------|------|------|--------|-------|
| Dungeon Crawler Carl | Matt Dinniman | | | Hall of Fame | Fast-paced, snarky, competence porn |
| Project Hail Mary | Andy Weir | | | Hall of Fame | Peak competence porn |
| Hyperion | Dan Simmons | | | Hall of Fame | Religious zealotry, AI factions |
| Altered Carbon | Richard Morgan | | | Hall of Fame | Gritty cyberpunk |
| Children of Time | Adrian Tchaikovsky | | | Hall of Fame | Truly alien aliens |
| Mistborn | Brandon Sanderson | | | Mixed Bag | Too YA, lacks grit |
| Blindsight | Peter Watts | | | Mixed Bag | Great premise, confusing execution |
| Foundation | Isaac Asimov | | | Bounces | Pacing slog |

---

## 0. Profile Bootstrap (UAP — User Attribute Prediction)

Before testing the pipeline we need the user profile in Windlass tag-score format.
The natural language profile in `user-profile-example.md` is the input.
This is a **UAP task** (User Attribute Prediction): deduce structured preferences from interaction history.

### Prompt (paste to Gemini)

```
Task: User Attribute Prediction — Profile Bootstrap

Convert a natural language reading profile into a structured Windlass preference profile.
Output numeric scores (-100 to +100) for each dimension.

Score meaning:
  +100 = absolute favourite / core identity
   +80 = strong preference
   +50 = likes
     0 = neutral / unknown
   -50 = dislikes
   -80 = strong dislike
  -100 = hard dealbreaker (constraint, not preference)

## Natural Language Profile
[paste contents of user-profile-example.md here]

## Dimension taxonomy (score every dimension you can infer; leave out unknowns)

### genre
hard_scifi, space_opera, epic_fantasy, cyberpunk, grimdark_fantasy, litRPG,
military_scifi, horror, cozy_mystery, romance, historical_fiction

### tone
grimdark, dry_wit, hopeful, bleak, comedic, satirical, tense, epic

### protagonist
highly_competent, underdog, ensemble, single_pov_intimate, anti_hero

### style
fast_paced, slow_burn, lore_density, world_building_heavy, character_driven,
puzzle_solving, hard_science_rigour

### content_warning
harm_to_children, sexual_content, torture, graphic_violence

### preference (queue meta — no item-side counterpart)
tonal_variance, series_completion_required, standalone_friendly

### author_affinity (positive only — list names with scores)

### user_constraints (hard dealbreakers — list what should become SQL WHERE filters)

## Output (JSON)
{
  "profile_signals": {
    "genre": {"hard_scifi": 0, "space_opera": 0, ...},
    "tone": {...},
    "protagonist": {...},
    "style": {...},
    "content_warning": {...},
    "preference": {...}
  },
  "author_affinity": [
    {"author": "Alastair Reynolds", "score": 90},
    ...
  ],
  "user_constraints": [
    {"constraint_type": "content_warning", "dimension_id": "harm_to_children", "reason": "hard dealbreaker"},
    {"constraint_type": "format", "dimension_id": "unfinished_series", "reason": "author milking dealbreaker"},
    ...
  ]
}
```

### Gemini Response

```json

```

### Findings

- Dimensions Gemini scored confidently vs left at 0:
- Any scores that feel wrong after review:
- Dealbreakers correctly captured in user_constraints:
- Dimensions missing from the taxonomy that the profile implied:

---

## 1. API Reachability

### Audnexus

Base URL: `https://api.audnex.us`

Endpoints used:
- Book by ASIN: `GET /books/{asin}`
- Author by ASIN: `GET /authors/{asin}`

#### Commands

```bash
# Fetch a book by ASIN
curl -s "https://api.audnex.us/books/{ASIN}" | jq .

# Fetch an author
curl -s "https://api.audnex.us/authors/{ASIN}" | jq .
```

#### Raw Responses

<!-- Paste curl output here per book -->

#### Findings

- Tags returned: 
- Tag format (comma-separated? array?): 
- Coverage gaps: 
- Fields useful for Stage 0.5: 
- Fields NOT returned that we expected: 

---

### Hardcover

Base URL: `https://hardcover.app/api/v1/graphql`

Auth: Bearer token required (set `HC_TOKEN` env var)

#### Commands

```bash
# Look up a book by ISBN or title
curl -s -X POST "https://hardcover.app/api/v1/graphql" \
  -H "Authorization: Bearer $HC_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "query { books(where: {title: {_ilike: \"%TITLE%\"}}, limit: 1) { id title slug genres { genre { name } } tags rating ratings_count } }"
  }' | jq .

# Look up by ISBN
curl -s -X POST "https://hardcover.app/api/v1/graphql" \
  -H "Authorization: Bearer $HC_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "query": "query { books(where: {book_mappings: {isbn_10: {_eq: \"ISBN10\"}}}, limit: 1) { id title genres { genre { name } } tags rating ratings_count } }"
  }' | jq .
```

#### Raw Responses

<!-- Paste curl output here per book -->

#### Findings

- Tags returned: 
- Tag format: 
- Coverage vs Audnexus: 
- Fields useful for Stage 0.5: 
- Fields NOT returned that we expected: 

---

## 2. Tag Normalization

### Problem
Community tags from APIs are noisy: `ya-dystopian`, `teen-dystopian`, `antiutopian` → all should map to one canonical slug.

### Tag Inventory

Raw tags collected from API responses:

| Raw tag (Audnexus) | Raw tag (Hardcover) | Proposed canonical slug | Band |
|--------------------|---------------------|------------------------|------|
|                    |                     |                        |      |

### Normalization Rules Discovered

<!-- Document ambiguous mappings, missing tags, tags that need splitting -->

### Findings

- Total unique raw tags seen: 
- Tags that map cleanly 1:1: 
- Tags that need merging: 
- Tags with no obvious canonical home: 
- Tags APIs never return (must come from Stage 1 LLM): 

---

## 3. Stage 0.5 — Structured Scoring

### Setup

Toy profile used (manually constructed — represents a user who likes grimdark fantasy and hard sci-fi):

```
genre:epic_fantasy       +80
genre:hard_scifi         +70
genre:horror             -40
tone:grimdark            +90
tone:dry_wit             +60
content_warning:sexual_content  -80
style:lore_density       +70
style:fast_paced         +50
```

### Scoring Procedure

For each book: decode its normalized tags into the profile space, compute dot product by hand (sum of matched dimension scores).

| Book | Matched tags | Dot product | Gate? (pass/reject) | Expected? |
|------|-------------|-------------|---------------------|-----------|
|      |             |             |                     |           |

### Findings

- Did the gate behave as expected? 
- Any false positives (passed gate but clearly wrong for profile)? 
- Any false negatives (rejected but should have passed)? 
- Tag coverage problem: how many books had <3 matching dimensions? 

---

## 4. Stage 1 — LLM Enrichment (TUI)

Task type: **Target User Identification** — given a book's attributes, identify the reader type most likely to enjoy it, then evaluate fit against a specific user profile.

### Prompt Template

```
Task: Target User Identification and Profile Fit Scoring

You are evaluating a book to determine which reader type would enjoy it, and then scoring its fit against a specific user's profile.

## Book
Title: {title}
Author: {author}
Series: {series}
Publisher description: {blurb}
Community tags (normalized): {tags_from_stage_0.5}
Narrator: {narrator}

## User Profile
{profile_dimensions_with_labels}

## Task
1. Identify the reader type for this book across these dimensions. Use only the dimension names listed — do not invent new ones.
   Dimensions: genre, tone, content_warning, protagonist, narrative_structure, style, arc

2. For each dimension you identify, score the book's fit against the user profile above on a scale of -100 to +100.

3. Flag any content warnings that are absolute dealbreakers (score: -100) vs soft dislikes.

## Output (JSON only, no prose)
{
  "reader_type_tags": {
    "genre": ["..."],
    "tone": ["..."],
    "content_warning": [],
    "protagonist": ["..."],
    "style": ["..."]
  },
  "profile_fit_scores": {
    "genre": 0,
    "tone": 0,
    "style": 0,
    "content_warning": 0
  },
  "dealbreaker": false,
  "dealbreaker_reason": null
}
```

### Runs

#### Book 1: {title}

**Input tags from Stage 0.5:**
```
(paste normalized tags)
```

**Prompt pasted to Gemini:** (mark if modified from template)

**Gemini response:**
```json

```

**Findings:**
- Tags LLM added beyond API: 
- Tags LLM got wrong: 
- Hallucinations: 
- Profile fit score sensible? 

---

## 5. Decide — Two-Pass

### Pass 1 — TKR Ranking Prompt Template

```
Task: Top-K Recommendation — Ranking Pass

You are selecting the single best next audiobook for a user from a candidate list.
Your job in this pass is ONLY to rank and select — do not write any prose description.

## User Profile (current mood: {mood})
{profile_dimensions_with_labels}
Author preferences: {author_prefs}

## Current Queue (books already listening to)
{queue_books_with_dominant_tags}

## Tonal variance preference: {tonal_variance_label}
(e.g. "binge reader — prefers sustained tonal consistency" or "variety reader — fatigues on similar tone")

## Candidates (randomly ordered)
{candidate_list_with_decoded_tags}

## Rules
- User is {N_GROUP|P_GROUP}: {niche/mainstream preference explanation}
- Queue variance rule: {instruction based on tonal_variance}
- Hard constraints already filtered — all candidates are safe to recommend

## Output (JSON only)
{
  "book_id": "...",
  "ranking_rationale": "One sentence: why this candidate over the others, referencing specific tag matches and queue contrast."
}
```

### Pass 2 — Blurb Generation Prompt Template

```
Task: Listening Pitch Generation

Write a personalised listening pitch for a specific audiobook recommendation.

## Book
Title: {title}
Author: {author}
Key tags: {top_5_tags}
Narrator: {narrator}

## User's top preferences right now
{top_5_profile_dimensions}
Current mood: {mood}

## Rules
- Maximum 50 words
- No star ratings, no numeric scores, no "5-star" language
- Qualitative, descriptive prose only
- Address why THIS book fits THIS user right now

## Output (JSON only)
{
  "reason": "..."
}
```

### Runs

#### Run 1

**Candidates used:**
```
(list books + their tag summaries)
```

**Pass 1 Gemini response:**
```json

```

**Pass 2 Gemini response:**
```json

```

**Findings:**
- Did Pass 1 pick make sense? 
- Did Pass 2 blurb feel personal or generic? 
- Did separating the passes help? 
- Anything the prompt got wrong? 

---

## 6. Learn — IR Profile Update

Task type: **Interest Recognition** — given a user's rated books and their tag profiles, update preference dimensions.

### Prompt Template

```
Task: Interest Recognition — Profile Update

A user has just finished an audiobook and submitted a rating. Update their preference profile based on this new data point.

## Rated Book
Title: {title}
Rating: {1-5} stars
Tags: {decoded_tag_vector}
Author: {author} (current author score: {author_score})

## Current Profile (relevant dimensions only)
{profile_dimensions_with_scores_and_labels}

## Rules
- Adjust scores by at most ±10 per rating event (avoid overreaction to a single book)
- A 5-star rating strengthens matching dimensions; a 1-star weakens them
- 3-star is neutral — only adjust dimensions that were strongly predicted and failed
- Author score: if rating ≥ 4, increase author affinity; also strengthen correlated style/genre dimensions
- Do not create new dimension types — only update existing ones

## Output (JSON only)
{
  "dimension_updates": [
    {"dimension_id": "genre:epic_fantasy", "delta": +5, "reason": "5-star confirms strong match"},
    ...
  ],
  "author_update": {"author": "...", "delta": +8},
  "style_transfer": [
    {"dimension_id": "style:lore_density", "delta": +3, "reason": "Sanderson affinity transfer"}
  ]
}
```

### Runs

#### Run 1: {title} — {rating} stars

**Book tags:**
```
(paste)
```

**Gemini response:**
```json

```

**Findings:**
- Delta magnitudes sensible? 
- Style transfer sensible? 
- Anything the LLM changed that it shouldn't have? 

---

## Summary of Findings

### What the spec got right

### What needs updating

### Open questions for next session

