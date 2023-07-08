use crate::packs::{
    parsing::{
        ruby::parse_utils::{
            fetch_casgn_name, fetch_const_const_name, fetch_const_name,
            fetch_node_location, get_definition_from,
            get_reference_from_active_record_association, loc_to_range,
        },
        ParsedDefinition, UnresolvedReference,
    },
    ProcessedFile,
};
use lib_ruby_parser::{
    nodes, traverse::visitor::Visitor, Node, Parser, ParserOptions,
};
use line_col::LineColLookup;
use std::{fs, path::Path};

struct ReferenceCollector<'a> {
    pub references: Vec<UnresolvedReference>,
    pub definitions: Vec<ParsedDefinition>,
    pub current_namespaces: Vec<String>,
    pub line_col_lookup: LineColLookup<'a>,
    pub behavioral_change_in_namespace: bool,
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
            self.visit(inner);
        }
        let definition_loc = fetch_node_location(&node.name).unwrap();
        let location = loc_to_range(definition_loc, &self.line_col_lookup);

        let definition = get_definition_from(
            &namespace,
            &self.current_namespaces,
            &location,
        );

        // Note – is there a way to use lifetime specifiers to get rid of this and
        // just keep current namespaces as a vector of string references or something else
        // more efficient?
        self.current_namespaces.push(namespace);

        if let Some(inner) = &node.body {
            self.visit(inner);
        }

        if self.behavioral_change_in_namespace {
            self.definitions.push(definition);
        }

        self.behavioral_change_in_namespace = false;

        self.current_namespaces.pop();
    }

    fn on_send(&mut self, node: &nodes::Send) {
        let association_reference =
            get_reference_from_active_record_association(
                node,
                &self.current_namespaces,
                &self.line_col_lookup,
            );

        if let Some(association_reference) = association_reference {
            self.references.push(association_reference);
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

        self.definitions.push(ParsedDefinition {
            fully_qualified_name,
            location: loc_to_range(&node.expression_l, &self.line_col_lookup),
        });

        if let Some(v) = node.value.to_owned() {
            self.visit(&v);
        } else {
            // We don't handle constant assignments as part of a multi-assignment yet,
            // e.g. A, B = 1, 2
            // See the documentation for nodes::Casgn#value for more info.
        }
    }

    fn on_module(&mut self, node: &nodes::Module) {
        let namespace = fetch_const_name(&node.name)
            .expect("We expect no parse errors in class/module definitions");
        let definition_loc = fetch_node_location(&node.name).unwrap();
        let location = loc_to_range(definition_loc, &self.line_col_lookup);

        let definition = get_definition_from(
            &namespace,
            &self.current_namespaces,
            &location,
        );

        // Note – is there a way to use lifetime specifiers to get rid of this and
        // just keep current namespaces as a vector of string references or something else
        // more efficient?
        self.current_namespaces.push(namespace);

        if let Some(inner) = &node.body {
            self.visit(inner);
        }

        if self.behavioral_change_in_namespace {
            self.definitions.push(definition);
        }

        self.behavioral_change_in_namespace = false;

        self.current_namespaces.pop();
    }

    fn on_const(&mut self, node: &nodes::Const) {
        let Ok(name) = fetch_const_const_name(node) else { return };

        let namespace_path = self
            .current_namespaces
            .clone()
            .into_iter()
            .filter(|namespace| namespace != &name)
            .collect::<Vec<String>>();

        self.references.push(UnresolvedReference {
            name,
            namespace_path,
            location: loc_to_range(&node.expression_l, &self.line_col_lookup),
        })
    }

    fn on_def(&mut self, node: &nodes::Def) {
        self.behavioral_change_in_namespace = true;
        lib_ruby_parser::traverse::visitor::visit_def(self, node);
    }
}

pub(crate) fn process_from_path(path: &Path) -> ProcessedFile {
    let contents = fs::read_to_string(path).unwrap_or_else(|_| {
        panic!("Failed to read contents of {}", path.to_string_lossy())
    });

    process_from_contents(contents, path)
}

pub(crate) fn process_from_contents(
    contents: String,
    path: &Path,
) -> ProcessedFile {
    let options = ParserOptions {
        buffer_name: "".to_string(),
        ..Default::default()
    };

    let lookup = LineColLookup::new(&contents);
    let parser = Parser::new(contents.clone(), options);
    let parse_result = parser.do_parse();

    let ast_option: Option<Box<Node>> = parse_result.ast;

    let ast = match ast_option {
        Some(some_ast) => some_ast,
        None => {
            return ProcessedFile {
                absolute_path: path.to_owned(),
                unresolved_references: vec![],
                definitions: vec![],
            }
        }
    };

    let mut collector = ReferenceCollector {
        references: vec![],
        current_namespaces: vec![],
        definitions: vec![],
        line_col_lookup: lookup,
        behavioral_change_in_namespace: false,
    };

    collector.visit(&ast);

    let unresolved_references = collector.references;

    let absolute_path = path.to_owned();

    // The packwerk parser uses a ConstantResolver constructed by constants inferred from the file system
    // see zeitwerk_utils for more.
    // For a parser that uses parsed constants, see the experimental parser
    let definitions = collector.definitions;

    ProcessedFile {
        absolute_path,
        unresolved_references,
        definitions,
    }
}
