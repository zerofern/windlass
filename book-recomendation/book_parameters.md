To successfully build your audiobook recommendation engine, you can mine a wealth of deep semantic and structural features from the full text. Based on the provided research, here is a detailed breakdown of exactly **what features you should track** and **how you can mine them** using modern Natural Language Processing (NLP) techniques.

### 1. Tracking Story Metrics

**A. Emotional Arcs**

- **What to track:** The overarching emotional trajectory of the narrative. Research identifies that the plots of complex stories generally follow one of six core emotional shapes: **'Rags to riches'** (a steady rise in sentiment), **'Tragedy'** (a steady fall), **'Man in a hole'** (a fall followed by a rise), **'Icarus'** (a rise followed by a fall), **'Cinderella'** (rise-fall-rise), or **'Oedipus'** (fall-rise-fall).
- **How to mine them:** You can extract these arcs using a sliding window approach combined with a dictionary-based sentiment analysis tool. Specifically, you can slide a uniform window of 10,000 words through the entire book, shifting it progressively to create a time series of the text. For each window, compute a sentiment score using a lexicon like the labMT dataset (utilized by the Hedonometer tool), which assigns happiness scores to words. Plotting these sequential scores reveals the macro-level emotional arc of the book.

**B. Character Sentences (CS)**

- **What to track:** Sentences that convey essential characterization cues. A "Character Sentence" is any sentence that provides descriptive or action-based information allowing a reader to infer a character's physical traits, mental states (beliefs, desires), emotional responses, or verbal and physical actions.
- **How to mine them:** This requires a specialized NLP pipeline integrating Large Language Models (LLMs). First, segment the text into distinct sentences. Then, use an LLM with a specific prompt framework to perform **coreference expansion**—meaning the model replaces ambiguous pronouns (like "he" or "she") with the actual character's name. To ensure accuracy, pass the output through a **dependency parsing filter** (such as the spaCy library) to verify the grammatical structure. This filter ensures that the character is the actual grammatical subject (the "doer") of the sentence, preventing the system from falsely attributing actions to characters who are merely receiving them.

**C. Relation Arcs**

- **What to track:** The dynamic, shifting emotional circumstances and interactions between directed pairs of characters as the plot unfolds across a sequence of events.
- **How to mine them:** First, utilize an NLP tool like BookNLP to extract character entities, followed by an event tagger (like a BiLSTM model) to extract specific narrative events. Apply **Semantic Role Labeling** to determine who is the "actor" and who is the "experiencer" in each event. Next, to capture the nuanced shifting circumstances between the characters, use a model like RoBERTa to assign a fine-grained sentiment score to the event, and a multi-label emotion classifier (trained on datasets like GoEmotions) to assign specific emotional undertones (e.g., anger, joy, fear). Finally, because event-by-event data is noisy, apply a smoothing mathematical function, such as a **Savitzky-Golay filter**, to plot a coherent "relation arc" over time.

### 2. Tracking Non-Story (Stylometric) Metrics

**A. Linguistic and Syntactic Features**

- **What to track:** The structural DNA of the author's writing style. Key lexical features include the average length of paragraphs, sentences, and words, as well as vocabulary richness (measured by type-token ratio). Syntactically, you should track the percentage distributions of specific parts of speech, such as adverbs, adjectives, nouns, verbs, and interrogatives. You can also track fiction-specific metadata, such as the percentage of the text dedicated to dialogue and the total number of unique characters or locations.
- **How to mine them:** These can be mined using standard NLP text-processing libraries. Use Part-Of-Speech (POS) taggers to count syntactical distributions. To count unique fictional characters and locations, use a fiction-aware Named Entity Recognizer (NER) such as LitNER, which is specifically trained to perform well on literary texts.

**B. Readability and Content Styles**

- **What to track:** How demanding the text is to read, alongside broad stylistic classifications. Track readability scores and the average number of syllables per word. For style, track where the book falls across six key stylistic dimensions: **literary vs. colloquial** (informal), **abstract vs. concrete** (physical objects), and **subjective vs. objective**.
- **How to mine them:** Readability can be computed easily using specific Python libraries like _textstat_ (for the Flesch reading ease score) and _pyphen_ (for syllable counts). The six stylistic dimensions can be measured using a tool like GutenTag, which utilizes a built-in tagger and a stylistic lexicon designed specifically for analyzing English literature.

**C. Psychological and Thematic Content**

- **What to track:** The underlying psychological themes and cognitive processes embedded in the vocabulary. Features to track include words representing **core drives and needs** (such as affiliation, achievement, power, rewards, and risk), biological processes, and perceptual processes (like words related to "seeing" or "feeling"). Interestingly, research shows that tracking these specific psychological words is highly effective for recommendations; for example, some users strongly prefer books with low frequencies of "core drives and needs" or "seeing" words.
- **How to mine them:** You can mine these using the **Linguistic Inquiry and Word Count (LIWC)** dictionary. The LIWC 2015 dictionary contains over 6,400 terms categorized into dozens of psychological and content sub-dictionaries. By calculating the percentage of a book's total words that fall into these specific LIWC categories, you can quantify the book's psychological and thematic footprint.

# numeric model

Yes, you can definitely consolidate all of these extracted features into a comprehensive numeric model. In computational stylistics and recommender systems, this is achieved by representing the book as a multi-dimensional **feature vector**.

By converting the text into a mathematical structure, your engine can seamlessly calculate similarities between books and align them with the user's numeric preference profile. Here is how you can build this numeric model using the story and non-story features:

### 1. Stylometric and Linguistic Vectors

You can build a dense numerical vector where each dimension directly corresponds to a specific stylistic, structural, or linguistic measurement. Researchers have successfully modeled literary books using numerical vectors containing over 100 distinct features.

- **Feature Frequencies:** You can represent a book as a vector of frequencies or percentages for specific syntactical patterns, such as the exact percentage of adjectives, adverbs, or interrogatives used in the text.
- **Lexical & Psychological Scores:** Your vector can include the continuous numeric scores for readability (like the Flesch reading ease score) and exact percentage values for psychological and thematic word categories extracted using tools like LIWC.

### 2. Semantic Text Embeddings

Instead of just counting features, you can use algorithms to translate the actual semantic meaning of the book's text and metadata into numeric space.

- **TF-IDF Vectors:** Textual data like the book's description, author, and user-generated tags can be transformed using the TF-IDF (Term Frequency-Inverse Document Frequency) algorithm. This evaluates the importance of a word relative to the entire corpus and generates a numerical vector for the book based on its relevant vocabulary.
- **Neural Network Embeddings:** To capture deep semantic relationships in long texts, you can represent the entire book using neural network embeddings like **Doc2vec**, which outputs a dense, fixed-length continuous vector representing the document. You can also use approaches like **AuthID**, which utilizes convolutional neural networks (CNNs) to learn numeric book representations directly from sequences of words or character bigrams.

### 3. Dimensionality Reduction for Story Arcs

An emotional arc generated by sliding a window across a book creates a time-series of thousands of data points. To make this usable in a numeric recommendation model, you must compress it.

- **Singular Value Decomposition (SVD):** You can use SVD to mathematically decompose the emotional time-series data of your corpus into an orthogonal basis of core emotional arcs (the primary "modes" like _Tragedy_ or _Rags to riches_).
- **Mode Coefficients:** Once decomposed, a single book's entire emotional trajectory can be represented numerically by its _mode coefficients_. This gives you a tiny vector of numbers (ranging from negative to positive) that indicates exactly how strongly the book's story aligns with each of the core emotional shapes.

### Using the Numeric Model for Recommendations

Once your book is represented as a combined numerical vector (merging the stylometric features, semantic embeddings, and SVD mode coefficients), your engine can mathematically compare it to other books or to the user's preference vector.

- **Cosine Similarity:** The system can calculate the angle between two multi-dimensional vectors in space. A cosine similarity score close to 1 indicates that two books are practically identical in their content, style, and emotional delivery, making it highly suitable for finding similar content.
- **Machine Learning Regressors:** These numeric vectors can be fed directly into machine learning algorithms like _k-Nearest Neighbors (kNN)_ or randomized regression trees (like _Extreme Trees_) to predict user ratings and rank the most relevant recommendations based on the user's current mood and long-term profile.
