use glob::glob;
use inflector::cases::classcase::to_class_case;
use lib_ruby_parser::{
    nodes, traverse::visitor::Visitor, Loc, Node, Parser, ParserOptions,
};
use line_col::LineColLookup;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

// This function takes a list (`namespace_nesting`) that represents
// the level of class and module nesting at a given location in code
// and outputs the value of `Module.nesting` at that location.
// This function may have bugs! Please provide your feedback.
// I hope to iterate on it to produce an accurate-to-spec implementation
// of `Module.nesting` given the current namespace. Some bugs may involve
// improving on how the input `namespace_nesting` is generated by the
// AST visitor.
//
// # Example:
// class Foo
//   module Bar
//     class Baz
//       puts Module.nesting.inspect
//     end
//   end
// end
// # inputs: ['Foo', 'Bar', 'Baz']
// # outputs: ['Foo::Bar::Baz', 'Foo::Bar', 'Foo']
fn calculate_module_nesting(namespace_nesting: &[String]) -> Vec<String> {
    let mut nesting = Vec::new();
    let mut previous = String::from("");
    namespace_nesting.iter().for_each(|namespace| {
        let new_nesting: String = if previous.is_empty() {
            namespace.to_owned()
        } else {
            format!("{}::{}", previous, namespace)
        };

        previous = new_nesting.to_owned();
        nesting.insert(0, new_nesting);
    });

    nesting
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuperclassReference {
    pub name: String,
    pub namespace_path: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub name: String,
    pub namespace_path: Vec<String>,
    pub location: Range,
}

impl Reference {
    fn possible_fully_qualified_constants(&self) -> Vec<String> {
        if self.name.starts_with("::") {
            return vec![self.name.to_owned()];
        }

        let mut possible_constants = vec![self.name.to_owned()];
        let module_nesting = calculate_module_nesting(&self.namespace_path);
        for nesting in module_nesting {
            let possible_constant = format!("::{}::{}", nesting, self.name);
            possible_constants.push(possible_constant);
        }

        possible_constants
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    pub fully_qualified_name: String,
    pub location: Range,
    pub namespace_path: Vec<String>,
}

#[derive(Debug, PartialEq, Copy, Eq, Serialize, Deserialize, Clone)]
pub struct Range {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub begin: usize,
    pub end: usize,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct LocationRange {
    pub start: Location,
    pub end: Location,
}

struct ReferenceCollector<'a> {
    pub references: Vec<Reference>,
    pub definitions: Vec<Definition>,
    pub current_namespaces: Vec<String>,
    pub line_col_lookup: LineColLookup<'a>,
    pub in_superclass: bool,
    pub superclasses: Vec<SuperclassReference>,
}

#[derive(Debug)]
enum ParseError {
    Metaprogramming,
    // Add more variants as needed for different error cases
}

fn fetch_node_location(node: &nodes::Node) -> Result<Loc, ParseError> {
    match node {
        Node::Const(const_node) => Ok(const_node.expression_l),
        node => {
            dbg!(node);
            panic!(
                "Cannot handle other node in get_constant_node_name: {:?}",
                node
            )
        }
    }
}

fn fetch_const_name(node: &nodes::Node) -> Result<String, ParseError> {
    match node {
        Node::Const(const_node) => Ok(fetch_const_const_name(const_node)?),
        Node::Cbase(_) => Ok(String::from("")),
        Node::Send(_) => Err(ParseError::Metaprogramming),
        Node::Lvar(_) => Err(ParseError::Metaprogramming),
        Node::Ivar(_) => Err(ParseError::Metaprogramming),
        Node::Self_(_) => Err(ParseError::Metaprogramming),
        node => {
            dbg!(node);
            panic!(
                "Cannot handle other node in get_constant_node_name: {:?}",
                node
            )
        }
    }
}

fn fetch_const_const_name(node: &nodes::Const) -> Result<String, ParseError> {
    match &node.scope {
        Some(s) => {
            let parent_namespace = fetch_const_name(s)?;
            Ok(format!("{}::{}", parent_namespace, node.name))
        }
        None => Ok(node.name.to_owned()),
    }
}

// TODO: Combine with fetch_const_const_name
fn fetch_casgn_name(node: &nodes::Casgn) -> Result<String, ParseError> {
    match &node.scope {
        Some(s) => {
            let parent_namespace = fetch_const_name(s)?;
            Ok(format!("{}::{}", parent_namespace, node.name))
        }
        None => Ok(node.name.to_owned()),
    }
}

impl<'a> Visitor for ReferenceCollector<'a> {
    fn on_class(&mut self, node: &nodes::Class) {
        // We're not collecting definitions, so no need to visit the class definitioname);
        let namespace_result = fetch_const_name(&node.name);
        // For now, we simply exit and stop traversing if we encounter an error when fetching the constant name of a class
        // We can iterate on this if this is different than the packwerk implementation
        if namespace_result.is_err() {
            return;
        }

        let namespace = namespace_result.unwrap();

        if let Some(inner) = node.superclass.as_ref() {
            // dbg!("Visiting superclass!: {:?}", inner);
            self.in_superclass = true;
            self.visit(inner);
            self.in_superclass = false;
        }

        let fully_qualified_name = if !self.current_namespaces.is_empty() {
            let mut name_components = self.current_namespaces.clone();
            name_components.push(namespace.to_owned());
            format!("::{}", name_components.join("::"))
        } else {
            format!("::{}", namespace)
        };

        // Note – is there a way to use lifetime specifiers to get rid of this and
        // just keep current namespaces as a vector of string references or something else
        // more efficient?
        self.current_namespaces.push(namespace);

        let definition_loc = fetch_node_location(&node.name).unwrap();
        let location = loc_to_range(definition_loc, &self.line_col_lookup);
        self.definitions.push(Definition {
            fully_qualified_name: fully_qualified_name.to_owned(),
            namespace_path: self.current_namespaces.to_owned(),

            location: location.to_owned(),
        });

        // Packwerk also considers a definition to be a "reference"
        self.references.push(Reference {
            name: fully_qualified_name,
            namespace_path: self.current_namespaces.to_owned(),
            location,
        });

        if let Some(inner) = &node.body {
            self.visit(inner);
        }

        self.current_namespaces.pop();
        self.superclasses.pop();
    }

    fn on_send(&mut self, node: &nodes::Send) {
        // TODO: Read in args, process associations as a separate class
        // These can get complicated! e.g. we can specify a class name
        // dbg!(&node);
        if node.method_name == *"has_one"
            || node.method_name == *"has_many"
            || node.method_name == *"belongs_to"
            || node.method_name == *"has_and_belongs_to_many"
        {
            let first_arg: Option<&Node> = node.args.get(0);
            let second_arg: Option<&Node> = node.args.get(1);

            if let Some(Node::Kwargs(kwargs)) = second_arg {
                for pair_node in kwargs.pairs.iter() {
                    if let Node::Pair(pair) = pair_node {
                        if let Node::Sym(k) = *pair.key.to_owned() {
                            if k.name.to_string_lossy() == *"class_name" {
                                if let Node::Str(v) = *pair.value.to_owned() {
                                    self.references.push(Reference {
                                        name: to_class_case(
                                            &v.value.to_string_lossy(),
                                        ),
                                        namespace_path: self
                                            .current_namespaces
                                            .to_owned(),
                                        location: loc_to_range(
                                            node.expression_l,
                                            &self.line_col_lookup,
                                        ),
                                    })
                                }
                            }
                        }
                    }
                }
            } else if let Some(Node::Sym(d)) = first_arg {
                self.references.push(Reference {
                    name: to_class_case(&d.name.to_string_lossy()),
                    namespace_path: self.current_namespaces.to_owned(),
                    location: loc_to_range(
                        node.expression_l,
                        &self.line_col_lookup,
                    ),
                })
            }
        }

        lib_ruby_parser::traverse::visitor::visit_send(self, node);
    }

    fn on_casgn(&mut self, node: &nodes::Casgn) {
        let name_result = fetch_casgn_name(node);
        if name_result.is_err() {
            return;
        }

        // TODO: This can be extracted from on_class
        let name = name_result.unwrap();
        let fully_qualified_name = if !self.current_namespaces.is_empty() {
            let mut name_components = self.current_namespaces.clone();
            name_components.push(name);
            format!("::{}", name_components.join("::"))
        } else {
            format!("::{}", name)
        };

        self.definitions.push(Definition {
            fully_qualified_name,
            namespace_path: self.current_namespaces.to_owned(),
            location: loc_to_range(node.expression_l, &self.line_col_lookup),
        });

        if let Some(v) = node.value.to_owned() {
            self.visit(&v);
        } else {
            // We don't handle constant assignments as part of a multi-assignment yet,
            // e.g. A, B = 1, 2
            // See the documentation for nodes::Casgn#value for more info.
        }
    }

    // TODO: extract the common stuff from on_class
    fn on_module(&mut self, node: &nodes::Module) {
        let namespace = fetch_const_name(&node.name)
            .expect("We expect no parse errors in class/module definitions");

        // EXTRACT THIS FROM ON_CLASS!
        let fully_qualified_name = if !self.current_namespaces.is_empty() {
            let mut name_components = self.current_namespaces.clone();
            name_components.push(namespace.to_owned());
            format!("::{}", name_components.join("::"))
        } else {
            format!("::{}", namespace)
        };

        // Note – is there a way to use lifetime specifiers to get rid of this and
        // just keep current namespaces as a vector of string references or something else
        // more efficient?
        self.current_namespaces.push(namespace);

        let definition_loc = fetch_node_location(&node.name).unwrap();
        let location = loc_to_range(definition_loc, &self.line_col_lookup);
        self.definitions.push(Definition {
            fully_qualified_name: fully_qualified_name.to_owned(),
            namespace_path: self.current_namespaces.to_owned(),

            location: location.to_owned(),
        });

        // Packwerk also considers a definition to be a "reference"
        self.references.push(Reference {
            name: fully_qualified_name,
            namespace_path: self.current_namespaces.to_owned(),
            location,
        });

        if let Some(inner) = &node.body {
            self.visit(inner);
        }

        self.current_namespaces.pop();
    }

    fn on_const(&mut self, node: &nodes::Const) {
        let Ok(name) = fetch_const_const_name(node) else { return };

        if self.in_superclass {
            self.superclasses.push(SuperclassReference {
                name: name.to_owned(),
                namespace_path: self.current_namespaces.to_owned(),
            })
        }
        // In packwerk, NodeHelpers.enclosing_namespace_path (erroneously) ignores
        // namespaces where a superclass OR namespace is the same as the current reference name
        let matching_superclass_option = self
            .superclasses
            .iter()
            .find(|superclass| superclass.name == name);

        let namespace_path =
            if let Some(matching_superclass) = matching_superclass_option {
                matching_superclass.namespace_path.to_owned()
            } else {
                self.current_namespaces
                    .clone()
                    .into_iter()
                    .filter(|namespace| {
                        namespace != &name
                            || self
                                .superclasses
                                .iter()
                                .any(|superclass| superclass.name == name)
                    })
                    .collect::<Vec<String>>()
            };

        self.references.push(Reference {
            name,
            namespace_path,
            location: loc_to_range(node.expression_l, &self.line_col_lookup),
        })
    }
}

fn loc_to_range(loc: Loc, lookup: &LineColLookup) -> Range {
    let (start_row, start_col) = lookup.get(loc.begin); // There's an off-by-one difference here with packwerk
    let (end_row, end_col) = lookup.get(loc.end);

    Range {
        start_row,
        start_col: start_col - 1,
        end_row,
        end_col,
    }
}

pub fn get_references(absolute_root: &Path) -> Vec<Reference> {
    // Later this can come from config
    let pattern = absolute_root.join("packs/**/*.rb");

    glob(pattern.to_str().unwrap())
        .expect("Failed to read glob pattern")
        .par_bridge() // Parallel iterator
        .flat_map(|entry| match entry {
            Ok(path) => extract_from_path(&path),
            Err(e) => {
                println!("{:?}", e);
                panic!("blah");
            }
        })
        .collect()
}

pub(crate) fn extract_from_path(path: &PathBuf) -> Vec<Reference> {
    let contents = fs::read_to_string(path).unwrap_or_else(|_| {
        panic!("Failed to read contents of {}", path.to_string_lossy())
    });

    extract_from_contents(contents)
}

fn extract_from_contents(contents: String) -> Vec<Reference> {
    let options = ParserOptions {
        buffer_name: "".to_string(),
        ..Default::default()
    };

    let lookup = LineColLookup::new(&contents);
    let parser = Parser::new(contents.clone(), options);
    let _ret = parser.do_parse();

    let ast_option: Option<Box<Node>> = _ret.ast;

    let ast = match ast_option {
        Some(some_ast) => some_ast,
        None => return vec![],
    };

    // .unwrap_or_else(|| panic!("No AST found for {}!", &path.display()));
    let mut collector = ReferenceCollector {
        references: vec![],
        current_namespaces: vec![],
        definitions: vec![],
        line_col_lookup: lookup,
        in_superclass: false,
        superclasses: vec![],
    };

    collector.visit(&ast);

    let mut definition_to_location_map: HashMap<String, Range> = HashMap::new();

    for d in collector.definitions {
        // if d.fully_qualified_name
        //     .contains("DormantAccountVerificationController")
        // {
        //     dbg!(&d);
        // }
        definition_to_location_map.insert(d.fully_qualified_name, d.location);
    }

    collector
        .references
        .into_iter()
        .filter(|r| {
            let mut should_ignore_local_reference = false;
            let possible_constants = r.possible_fully_qualified_constants();
            for constant_name in possible_constants {
                if let Some(location) = definition_to_location_map
                    .get(&constant_name)
                    .or(definition_to_location_map
                        .get(&format!("::{}", constant_name)))
                {
                    let reference_is_definition = location.start_row
                        == r.location.start_row
                        && location.start_col == r.location.start_col;
                    // In lib/packwerk/parsed_constant_definitions.rb, we don't count references when the reference is in the same place as the definition
                    // This is an idiosyncracy we are porting over here for behavioral alignment, although we might be doing some unnecessary work.
                    if reference_is_definition {
                        should_ignore_local_reference = false
                    } else {
                        should_ignore_local_reference = true
                    }
                }
            }
            !should_ignore_local_reference
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trivial_case() {
        let contents: String = String::from("Foo");
        assert_eq!(
            vec![Reference {
                name: String::from("Foo"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 0,
                    end_row: 1,
                    end_col: 4
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_nested_constant() {
        let contents: String = String::from("Foo::Bar");
        assert_eq!(
            vec![Reference {
                name: String::from("Foo::Bar"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 0,
                    end_row: 1,
                    end_col: 9
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_deeply_nested_constant() {
        let contents: String = String::from("Foo::Bar::Baz");
        assert_eq!(
            vec![Reference {
                name: String::from("Foo::Bar::Baz"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 0,
                    end_row: 1,
                    end_col: 14
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_very_deeply_nested_constant() {
        let contents: String = String::from("Foo::Bar::Baz::Boo");
        assert_eq!(
            vec![Reference {
                name: String::from("Foo::Bar::Baz::Boo"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 0,
                    end_row: 1,
                    end_col: 19
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_class_definition() {
        let contents: String = String::from(
            "\
class Foo
end
            ",
        );

        assert_eq!(
            vec![Reference {
                name: String::from("::Foo"),
                namespace_path: vec![String::from("Foo")],
                location: Range {
                    start_row: 1,
                    start_col: 6,
                    end_row: 1,
                    end_col: 10
                }
            }],
            extract_from_contents(contents)
        );
    }

    #[test]
    fn test_class_namespaced_constant() {
        let contents: String = String::from(
            "\
class Foo
  Bar
end
        ",
        );

        assert_eq!(
            Reference {
                name: String::from("Bar"),
                namespace_path: vec![String::from("Foo")],
                location: Range {
                    start_row: 2,
                    start_col: 2,
                    end_row: 2,
                    end_col: 6
                }
            },
            *extract_from_contents(contents).get(1).unwrap()
        );
    }

    #[test]
    fn test_deeply_class_namespaced_constant() {
        let contents: String = String::from(
            "\
class Foo
  class Bar
    Baz
  end
end
        ",
        );

        assert_eq!(
            Reference {
                name: String::from("Baz"),
                namespace_path: vec![String::from("Foo"), String::from("Bar")],
                location: Range {
                    start_row: 3,
                    start_col: 4,
                    end_row: 3,
                    end_col: 8
                }
            },
            *extract_from_contents(contents).get(2).unwrap()
        );
    }

    #[test]
    fn test_very_deeply_class_namespaced_constant() {
        let contents: String = String::from(
            "\
class Foo
  class Bar
    class Baz
      Boo
    end
  end
end
        ",
        );

        assert_eq!(
            Reference {
                name: String::from("Boo"),
                namespace_path: vec![
                    String::from("Foo"),
                    String::from("Bar"),
                    String::from("Baz")
                ],
                location: Range {
                    start_row: 4,
                    start_col: 6,
                    end_row: 4,
                    end_col: 10
                }
            },
            *extract_from_contents(contents).get(3).unwrap()
        );
    }

    #[test]
    fn test_module_namespaced_constant() {
        let contents: String = String::from(
            "\
module Foo
  Bar
end
        ",
        );

        assert_eq!(
            vec![
                Reference {
                    name: String::from("::Foo"),
                    namespace_path: vec![String::from("Foo")],
                    location: Range {
                        start_row: 1,
                        start_col: 7,
                        end_row: 1,
                        end_col: 11
                    }
                },
                Reference {
                    name: String::from("Bar"),
                    namespace_path: vec![String::from("Foo")],
                    location: Range {
                        start_row: 2,
                        start_col: 2,
                        end_row: 2,
                        end_col: 6
                    }
                }
            ],
            extract_from_contents(contents),
        );
    }

    #[test]
    fn test_deeply_module_namespaced_constant() {
        let contents: String = String::from(
            "\
module Foo
  module Bar
    Baz
  end
end
        ",
        );

        assert_eq!(
            Reference {
                name: String::from("Baz"),
                namespace_path: vec![String::from("Foo"), String::from("Bar")],
                location: Range {
                    start_row: 3,
                    start_col: 4,
                    end_row: 3,
                    end_col: 8
                }
            },
            *extract_from_contents(contents).get(2).unwrap()
        );
    }

    #[test]
    fn test_very_deeply_module_namespaced_constant() {
        let contents: String = String::from(
            "\
module Foo
  module Bar
    module Baz
      Boo
    end
  end
end
        ",
        );

        assert_eq!(
            Reference {
                name: String::from("Boo"),
                namespace_path: vec![
                    String::from("Foo"),
                    String::from("Bar"),
                    String::from("Baz")
                ],
                location: Range {
                    start_row: 4,
                    start_col: 6,
                    end_row: 4,
                    end_col: 10
                }
            },
            *extract_from_contents(contents).get(3).unwrap()
        );
    }

    #[test]
    fn test_mixed_namespaced_constant() {
        let contents: String = String::from(
            "\
class Foo
  module Bar
    class Baz
      Boo
    end
  end
end
        ",
        );

        assert_eq!(
            Reference {
                name: String::from("Boo"),
                namespace_path: vec![
                    String::from("Foo"),
                    String::from("Bar"),
                    String::from("Baz")
                ],
                location: Range {
                    start_row: 4,
                    start_col: 6,
                    end_row: 4,
                    end_col: 10
                },
            },
            *extract_from_contents(contents).get(3).unwrap()
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_compact_style_class_definition_constant() {
        let contents: String = String::from(
            "\
class Foo::Bar
  Baz
end
        ",
        );

        assert_eq!(
            Reference {
                name: String::from("Baz"),
                namespace_path: vec![String::from("Foo::Bar")],
                location: Range {
                    start_row: 2,
                    start_col: 2,
                    end_row: 2,
                    end_col: 6
                }
            },
            *extract_from_contents(contents).get(1).unwrap(),
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_compact_style_with_module_constant() {
        let contents: String = String::from(
            "\
class Foo::Bar
  module Baz
  end
end
        ",
        );

        assert_eq!(
            Reference {
                name: String::from("::Foo::Bar::Baz"),
                namespace_path: vec![
                    String::from("Foo::Bar"),
                    String::from("Baz")
                ],
                location: Range {
                    start_row: 2,
                    start_col: 9,
                    end_row: 2,
                    end_col: 13
                }
            },
            *extract_from_contents(contents).get(1).unwrap()
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_array_of_constant() {
        let contents: String = String::from("[Foo]");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 1);
        let reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("Foo"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 1,
                    end_row: 1,
                    end_col: 5
                }
            },
            *reference
        );
    }
    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_array_of_multiple_constants() {
        let contents: String = String::from("[Foo, Bar]");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 2);
        let reference1 = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("Foo"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 1,
                    end_row: 1,
                    end_col: 5
                }
            },
            *reference1
        );
        let reference2 = references
            .get(1)
            .expect("There should be a reference at index 1");
        assert_eq!(
            Reference {
                name: String::from("Bar"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 6,
                    end_row: 1,
                    end_col: 10
                }
            },
            *reference2,
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_array_of_nested_constant() {
        let contents: String = String::from("[Baz::Boo]");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 1);
        let reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("Baz::Boo"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 1,
                    end_row: 1,
                    end_col: 10
                }
            },
            *reference,
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_globally_referenced_constant() {
        let contents: String = String::from("::Foo");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 1);
        let reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("::Foo"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 0,
                    end_row: 1,
                    end_col: 6
                }
            },
            *reference,
        );
    }

    #[test]
    // https://www.rubydoc.info/gems/rubocop/RuboCop/Cop/Style/ClassAndModuleChildren
    fn test_metaprogrammatically_referenced_constant() {
        let contents: String = String::from("described_class::Foo");
        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 0);
    }

    #[test]
    fn test_ignore_local_constant() {
        let contents: String = String::from(
            "\
class Foo
  BAR = 1
  def use_bar
    puts BAR
  end
end
        ",
        );

        assert_eq!(
            extract_from_contents(contents),
            vec![Reference {
                name: String::from("::Foo"),
                namespace_path: vec![String::from("Foo")],
                location: Range {
                    start_row: 1,
                    start_col: 6,
                    end_row: 1,
                    end_col: 10
                }
            }]
        )
    }

    #[test]
    fn test_ignore_local_constant_under_nested_module() {
        let contents: String = String::from(
            "\
class Foo
  class Baz
    BAR = 1
    def use_bar
      puts BAR
    end
  end
end
        ",
        );

        assert_eq!(
            extract_from_contents(contents),
            vec![
                Reference {
                    name: String::from("::Foo"),
                    namespace_path: vec![String::from("Foo"),],
                    location: Range {
                        start_row: 1,
                        start_col: 6,
                        end_row: 1,
                        end_col: 10
                    }
                },
                Reference {
                    name: String::from("::Foo::Baz"),
                    namespace_path: vec![
                        String::from("Foo"),
                        String::from("Baz")
                    ],
                    location: Range {
                        start_row: 2,
                        start_col: 8,
                        end_row: 2,
                        end_col: 12
                    }
                }
            ]
        );
    }

    #[test]
    fn test_super_classes_are_references() {
        let contents: String = String::from(
            "\
class Foo < Bar
end
        ",
        );

        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 2);
        let first_reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("Bar"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 12,
                    end_row: 1,
                    end_col: 16
                }
            },
            *first_reference,
        );
    }

    #[test]
    fn test_compact_nested_classes_are_references() {
        let contents: String = String::from(
            "\
class Foo::Bar
end
        ",
        );

        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 1);
        let first_reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("::Foo::Bar"),
                namespace_path: vec![String::from("Foo::Bar")],
                location: Range {
                    start_row: 1,
                    start_col: 6,
                    end_row: 1,
                    end_col: 15
                }
            },
            *first_reference,
        );
    }

    #[test]
    fn test_regular_nested_classes_are_references() {
        let contents: String = String::from(
            "\
class Foo
  class Bar
  end
end
        ",
        );

        let references: Vec<Reference> = extract_from_contents(contents);
        assert_eq!(
            references,
            vec![
                Reference {
                    name: String::from("::Foo"),
                    namespace_path: vec![String::from("Foo")],
                    location: Range {
                        start_row: 1,
                        start_col: 6,
                        end_row: 1,
                        end_col: 10
                    }
                },
                Reference {
                    name: String::from("::Foo::Bar"),
                    namespace_path: vec![
                        String::from("Foo"),
                        String::from("Bar")
                    ],
                    location: Range {
                        start_row: 2,
                        start_col: 8,
                        end_row: 2,
                        end_col: 12
                    }
                }
            ]
        );
    }
    #[test]
    fn test_const_assignments_are_references() {
        let contents: String = String::from(
            "\
FOO = BAR
",
        );
        let references: Vec<Reference> = extract_from_contents(contents);

        assert_eq!(references.len(), 1);
        let first_reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("BAR"),
                namespace_path: vec![],
                location: Range {
                    start_row: 1,
                    start_col: 6,
                    end_row: 1,
                    end_col: 10
                }
            },
            *first_reference
        )
    }

    #[test]
    fn test_has_one_association() {
        let contents: String = String::from(
            "\
class Foo
  has_one :some_user_model
end
        ",
        );

        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 2);
        let first_reference = references
            .get(1)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("SomeUserModel"),
                namespace_path: vec![String::from("Foo")],
                location: Range {
                    start_row: 2,
                    start_col: 2,
                    end_row: 2,
                    end_col: 27
                }
            },
            *first_reference,
        );
    }

    #[test]
    fn test_has_one_association_with_class_name() {
        let contents: String = String::from(
            "\
class Foo
  has_one :some_user_model, class_name: 'User'
end
        ",
        );

        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 2);
        let first_reference = references
            .get(1)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("User"),
                namespace_path: vec![String::from("Foo")],
                location: Range {
                    start_row: 2,
                    start_col: 2,
                    end_row: 2,
                    end_col: 47
                }
            },
            *first_reference,
        );
    }

    #[test]
    fn test_has_many_association() {
        let contents: String = String::from(
            "\
class Foo
  has_many :some_user_models
end
        ",
        );

        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 2);
        let first_reference = references
            .get(1)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("SomeUserModel"),
                namespace_path: vec![String::from("Foo")],
                location: Range {
                    start_row: 2,
                    start_col: 2,
                    end_row: 2,
                    end_col: 29
                }
            },
            *first_reference,
        );
    }

    #[test]
    fn test_it_uses_the_namespace_of_inherited_class_when_referencing_inherited_class(
    ) {
        let contents: String = String::from(
            "\
class Foo < Bar
  Bar
end
        ",
        );

        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 3);
        let first_reference = references
            .get(2)
            .expect("There should be a reference at index 0");
        assert_eq!(
            Reference {
                name: String::from("Bar"),
                namespace_path: vec![],
                location: Range {
                    start_row: 2,
                    start_col: 2,
                    end_row: 2,
                    end_col: 6
                }
            },
            *first_reference,
        );
    }

    #[test]
    fn test_it_ignores_locally_defined_nested_constants() {
        let contents: String = String::from(
            "\
class Foo
  class Bar
    Foo::Bar
  end
end
        ",
        );

        let references = extract_from_contents(contents);
        assert_eq!(references.len(), 2);
        let first_reference = references
            .get(0)
            .expect("There should be a reference at index 0");
        let second_reference = references
            .get(1)
            .expect("There should be a reference at index 0");

        assert_eq!(first_reference.name, String::from("::Foo"));
        assert_eq!(second_reference.name, String::from("::Foo::Bar"));
    }
}
