# Research References: Audiobook & Ebook Recommendation Engine

## 1. Emotional Arcs & Narrative Structure

Papers on quantifying the emotional shape and feel of a story over time.

- **"A Novel Method for Detecting Plot"** — Matthew L. Jockers (2014).
  Introduces the `syuzhet` R package. Uses low-pass filters and Fourier transforms on sentence-level sentiment to extract the underlying emotional arc ("shape") of a narrative.

- **"The Emotional Arcs of Stories are Dominated by Six Basic Shapes"** — Reagan, Mitchell, Kiley, Danforth & Dodds (2016).
  _EPJ Data Science._ NLP sentiment analysis on 1,700+ Project Gutenberg books. Mathematically validates Kurt Vonnegut's thesis that all stories reduce to ~6 emotional arc shapes (e.g., "Man in a Hole", "Cinderella").

---

## 2. Stylometry & Computational Literary Analysis

Papers on capturing an author's unique stylistic "fingerprint" and quantifying writing style.

- **"The Bestseller Code: Anatomy of the Blockbuster Novel"** — Jodie Archer & Matthew L. Jockers (2016).
  Commercial book (not peer-reviewed), but built an algorithm analyzing 20,000 novels. Extracted lexical density, sentence variance, pacing, and emotional rhythm to predict NYT Bestseller status with ~80% accuracy — _ignoring plot content entirely_.

- **Works by Shlomo Argamon & Moshe Koppel** — (various, search Google Scholar).
  Pioneers of automated authorship attribution. Their papers establish that **function word frequency** (glue words: _of, the, but, and_) combined with structural syntax trees is the most accurate way to capture a writing voice without topic pollution.

---

## 3. Text-Based & Style-Aware Recommender Systems

Papers on using stylistic and structural NLP features (not content/topic) to power book recommendations.

- **"Content-Based Book Recommendation using Stylistic Features"** — (various authors, search arXiv/Google Scholar).
  Multiple papers with this or similar titles. Addresses the cold-start problem with collaborative filtering and proposes stylometric features (lexical, syntactic, structural) as primary recommendation vectors.

- **"Context-Dependent User Modeling for Recommender Systems"** — Linas Baltrunas.
  Foundational paper on **User Splitting** / **Micro-Profiling**. Mathematically proves that splitting a single user into contextual sub-profiles (e.g., `User_Commute`, `User_Bedtime`) dramatically reduces prediction error vs. a single global profile.

---

## 4. Story Content Classification & Semantic Embeddings

Papers on classifying _what_ a book is about using modern NLP techniques.

- **"Sentence-BERT: Sentence Embeddings using Siamese BERT-Networks"** — Reimers & Gurevych.
  The foundational paper for modern semantic text similarity. Uses siamese/triplet network structures to produce semantically meaningful sentence embeddings (e.g., open-source models on Hugging Face like `all-MiniLM-L6-v2`).

- **"BERTopic: Neural topic modeling with a class-based TF-IDF procedure"** — Maarten Grootendorst.
  Modern topic modeling leveraging transformers + c-TF-IDF. Produces dense, interpretable topic clusters. Supports UMAP for dimensionality reduction. Supersedes classic LDA for literary theme extraction.

- **"MARCUS: An Event-Centric NLP Pipeline that generates Character Arcs from Narratives"** — (2025/2026).
  Bleeding-edge paper. Extracts events, participant characters, and implied sentiment to model inter-character relations and identify literary tropes (e.g., "Enemies to Lovers"). Addresses trope classification across genres.

- **"Character Sentences as Quantitative Metrics: Leveraging Large Language Models to Measure Literary Characterization"** — Qilin Liu (UBC).
  Explores using LLMs and NLP pipelines to automate character extraction and measure how characters are written.

- **"Using full-text content to characterize and identify best seller books: A study of early 20th-century literature"** — (PMC / PubMed Central).
  Combines full-text content with machine learning to assess whether text alone can predict commercial success. Conclusion: fiction predicts higher sales intensity than non-fiction, but pure text is highly unpredictable without external metadata.

- **Workshop on Narrative Understanding** — Association for Computational Linguistics (ACL).
  Proceedings contain papers on NLP event extraction models capturing causal logic between narrative events. Search ACL Anthology.

---

## 5. User Reading Behaviour & Implicit Feedback

Papers on using reading telemetry (session logs, velocity, abandonment) as implicit preference signals.

- **"E-Book Reading Practices in Different Subject Areas: An Exploratory Log Analysis"** — (search Google Scholar).
  Demonstrates that tracking session count/duration and consecutive pages reveals distinct reading patterns that explicit surveys/reviews completely miss.

- **"Automated Recommendations... Using Reading Activity Data"** — (2024, APSCE).
  Uses LLMs to interpret detailed reading logs (relative reading time per section) to extract a "vector of reading preferences."

- **Session-Based Recommendation Systems (SBRS)** — search keyword.
  Research area using RNNs or Transformers (e.g., **BERT4Rec**) to predict the next action from a sequence of past actions. Applied to reading: predict chapter completion, session length, and abandonment risk.

---

## 6. Mood Tracking & Affective Computing

Papers on inferring and recording user mood — both explicitly (UI) and implicitly (telemetry).

- **"MoodScope: Building a Mood Sensor from Smartphone Usage Patterns"** — Li et al., Microsoft Research.
  Inferred user mood with up to **93% accuracy** purely from smartphone telemetry (typing speed, app-switching, communication frequency) — no explicit user input required. Blueprint for turning reading velocity and rewind-rates into implicit mood signals.

- **"MoodSense: A Browser-Based Ensemble Sentiment Analysis System for Real-Time Mood Tracking"** — (2025, ResearchGate).
  Runs NLP sentiment models in-browser for real-time mood tracking. Good architectural reference for lightweight, self-hosted systems.

- **"Emotionally adaptive support: a narrative review of affective computing"** — (2025).
  Broad survey of how systems use interaction telemetry (audiovisual + behavioral signals) to build emotionally adaptive interfaces. Bridges raw computational emotion modeling and real user engagement.

- **"Mobile apps for mood tracking: an analysis of features and user reviews"** — Zhao et al.
  Analyzed features of commercial mood-tracking apps to identify what works for "collection and reflection" stages without causing user fatigue. Essential reading before designing explicit mood UI.

- **MOODMATE framework** — (search recent literature).
  Architecture that maps emotional data (tracked via UI or implicitly inferred) directly to a vector database to fetch mood-congruent media.

---

## 7. Context-Aware Recommender Systems (CARS) & Correcting Rating Bias

Papers on decoupling a book's quality from the user's emotional state at review time.

- **"The Role of Emotions in Context-aware Recommendation"** — Yong Zheng et al.
  Explores how emotions interact with recommendation algorithms. Outlines pre-filtering, post-filtering, and contextual modeling to prevent temporary emotional states from permanently skewing the rating matrix.

- **"Reducing Recommender System Biases: An Investigation of Rating Display Designs"** — (2024/2025).
  Examines system-induced anchoring bias (how showing a rating before a review changes user perception). Practical guide to designing review UIs that minimize cognitive bias.

- **"Don't classify ratings of affect; rank them!"** — (search Affective Computing literature).
  Proves that translating user ratings into relative ranks is far more reliable than absolute 1–5 star scales across different moods. Key insight: ask "Did the user rank this _higher than Book B while sad_?" not "Is this a 5-star book?"

- **Linas Baltrunas — "Item Splitting / User Splitting"** — (search Google Scholar).
  Companion work to his context-dependent user modeling. De-biases ratings by splitting a single user into multiple contextual profiles (e.g., separate rating matrix for "stressed" vs. "relaxed" contexts).

---

## 8. LLMs in Recommender Systems (LLM4Rec)

Cutting-edge papers (2024–2026) on using Large Language Models in recommendation pipelines.

### Foundational Surveys

- **"A Survey on Large Language Models for Recommendation"** — (arXiv, 2023/2024).
  Definitive taxonomy paper. Divides the field into:
  - **Discriminative LLMs (DLLM4Rec):** BERT-style models to extract features/embeddings.
  - **Generative LLMs (GLLM4Rec):** GPT-style models to directly generate the recommendation.

- **"Large Language Models for Generative Recommendation: A Survey and Visionary Discussions"** — (COLING 2024).
  Discusses flattening the traditional multi-stage pipeline (retrieval → filtering → ranking) into a single generative LLM step.

### Implicit Feedback & Temporal Preferences

- **"Using LLMs to Capture Users' Temporal Context for Recommendation"** — (RecSys CARS Workshop, 2025).
  Blueprint for session-based telemetry. LLMs disentangle short-term transient interests (binge behavior, current mood) from long-term stable tastes, generating two distinct natural-language user profiles that are then converted to embeddings.

- **"Extracting Implicit User Preferences in Conversational Recommender Systems Using Large Language Models"** — (IDEAS, 2025).
  Uses LLMs to parse unstructured user behavior and extract implied preferences, then passes them through a BERT-based multi-label classifier to produce fine-grained numerical values for the recommendation matrix.

### Context-Aware & Emotion-Driven Recommendation

- **"CORES: Context-Aware Emotion-Driven Recommendation System-Based LLM..."** — (MDPI, 2025).
  Combines LLMs (GPT-4) with BERT sentence transformers. Actively integrates the user's current emotional state, context (time/location), and item features into the recommendation embedding.

- **"Contextualizing Recommendation Explanations with LLMs: A User Study"** — (arXiv, 2025).
  Shows that using LLMs to generate personalized explanations (referencing the user's past behavior and current cognitive/affective needs) significantly increases user trust and consumption intention.

### Debiasing

- **"Bridging Semantic Understanding and Popularity Bias with LLMs"** — (arXiv, 2026).
  Proves that simply prompting an LLM to "ignore popular books" fails. Proposes frameworks to explicitly ground the LLM in the user's niche preference semantics rather than global popularity bias.

- **"Towards Fair Large Language Model-based Recommender Systems without Costly Retraining"** — (arXiv, 2026).
  Introduces **FUDLR**, a machine unlearning framework to dynamically debias a system based on fairness metrics without retraining the underlying embedding models.

---

## Useful Search Terms

For finding additional papers on Google Scholar / arXiv / ACL Anthology:

```
"stylometry" AND "recommender systems"
"computational literary studies" OR "cultural analytics"
"affective trajectories" AND "narrative"
"content-based recommendation" AND "stylistic features" AND "NLP"
"implicit feedback" AND "reading behavior" AND "recommender systems"
"affect-aware recommender systems" AND "rating bias"
"contextual rating bias" AND "collaborative filtering"
"mood-congruent bias" AND "evaluation bias"
LLM4Rec
DLLM4Rec
GLLM4Rec
"Multi-Phenomenon Datasets for Computational Literary Analysis"
"emotion-aware music sentiment systems"
"session-based recommendation" AND "BERT4Rec"
```
