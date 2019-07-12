package main

import (
	"database/sql"
	"errors"
	"log"
	"os"

	"github.com/go-sql-driver/mysql"
	"github.com/gotk3/gotk3/glib"
	"github.com/gotk3/gotk3/gtk"
)

const appID = "io.zippa.db"

func main() {
	application, err := gtk.ApplicationNew(appID, glib.APPLICATION_FLAGS_NONE)
	if err != nil {
		log.Fatal("Could not create application: ", err)
	}

	application.Connect("startup", func() {
		//
	})

	// Create initial window on activation
	application.Connect("activate", func() {
		win := createAppWindow(application)
		win.Show()
		application.AddWindow(win)
	})

	application.Connect("shutdown", func() {
		//
	})

	// Run Gtk application
	os.Exit(application.Run(os.Args))
}

func createAppWindow(application *gtk.Application) *gtk.ApplicationWindow {
	// Get the GtkBuilder UI definition in the glade file.
	// TODO: find a clean way of determining the absolute path to the UI
	// directory regardless of the current working directory on startup.
	builder, err := gtk.BuilderNewFromFile("ui/app-window.ui")
	if err != nil {
		log.Fatal("Could not initialize builder from glade file: ", err)
	}

	// Map the handlers to callback functions, and connect the signals to the
	// Builder.
	signals := map[string]interface{}{
		"on_connect": func() {
			config := mysql.NewConfig()
			config.User = builderObjectText(builder, "input_user")
			config.Passwd = builderObjectText(builder, "input_password")
			config.Net = "tcp"
			config.Addr = builderObjectText(builder, "input_host") + ":3306"
			onConnect(config)
		},
	}
	builder.ConnectSignals(signals)

	obj, err := builder.GetObject("app_window")
	if err != nil {
		log.Fatal("Could not get window instance from builder: ", err)
	}

	win, err := isAppWindow(obj)
	if err != nil {
		log.Panic(err)
	}
	return win
}

func isAppWindow(obj glib.IObject) (*gtk.ApplicationWindow, error) {
	if win, ok := obj.(*gtk.ApplicationWindow); ok {
		return win, nil
	}
	return nil, errors.New("not a *gtk.ApplicationWindow")
}

func builderObjectText(builder *gtk.Builder, id string) string {
	obj, err := builder.GetObject(id)
	if err != nil {
		log.Fatal("Could not get object from builder: ", err)
	}
	if entry, ok := obj.(*gtk.Entry); ok {
		s, err := entry.GetText()
		if err != nil {
			log.Fatal("Failed to get text from entry")
		}
		return s
	}
	log.Fatal("Object is not an entry.")
	return ""
}

func onConnect(config *mysql.Config) {
	// Open test DB connection
	db, err := sql.Open("mysql", config.FormatDSN())
	if err != nil {
		log.Fatal("Unable to open SQL instance: ", err)
	}
	err = db.Ping()
	if err != nil {
		log.Fatal("Unable to connect to DB server: ", err)
	}

	// Run a test query
	// This highlights a major limitation to using Go, since we have to know
	// the types for each column in the result set ahead of time...
	// There should be reasonable ways of working with this, but it'll take
	// some research.
	rows, err := db.Query("SHOW DATABASES")
	if err != nil {
		log.Fatal("Unable to run query: ", err)
	}
	var str string
	defer rows.Close()
	for rows.Next() {
		err := rows.Scan(&str)
		if err != nil {
			log.Fatal(err)
		}
		log.Println(str)
	}
}
